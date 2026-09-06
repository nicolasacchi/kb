//! `kb comments watch --path <artifact|folder> [--json] [--once]
//! [--timeout SECS] [--backlog] [--kb NAME]` — Claude-Code-friendly,
//! SSE-driven monitor for new `you`-authored review activity, scoped to
//! one artifact or a whole folder. It surfaces two things:
//!   - new open top-level comments authored by `you`, and
//!   - new replies authored by `you` on **any** comment — the user
//!     responding to Claude, including via a quick-response button.
//!
//! Together they make the loop two-way: Claude posts a comment/choice,
//! the user taps/replies, watch surfaces it, Claude answers in-session.
//!
//! The daemon's `/api/events` stream has no per-kb/per-artifact filter
//! (the `?filter=` query only honours `run:`), so scoping happens
//! client-side here: we resolve `--path` to a set of artifact ids once
//! at startup, subscribe to `comments.updated`, and on each event for an
//! in-scope artifact we GET its review file and surface any item whose
//! key we haven't seen before. That diff against a "seen" set is what
//! makes the loop echo-safe — Claude's own `reply` is `author: claude`
//! (skipped) and its `resolve` adds no new `you` item, so neither makes
//! the loop react to itself.
//!
//! Designed to be driven inside a Claude `/loop`: `--once` exits after
//! the first surfaced comment (one comment per loop turn), `--timeout`
//! caps the idle wait so a quiet stretch ends the turn cleanly.

use crate::sse::{open_events_stream, FrameReader};
use anyhow::{anyhow, Context, Result};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tokio::time::Instant;

/// Backoff schedule on disconnect — mirrors `kb push`.
const BACKOFF_STEPS: &[u64] = &[1, 2, 4, 8, 16, 30];
/// A connection up at least this long resets the backoff on a clean close.
const STABLE_SESSION: Duration = Duration::from_secs(5);

/// Dedupe key for a surfaced item. Comments key on `(aid, comment_id)`;
/// replies on `(aid, reply_id)`. An explicit enum (rather than a
/// `(aid, cid, reply_id)` triple with an empty-string sentinel for "no
/// reply") keeps the two namespaces distinct and collision-free.
#[derive(Clone, PartialEq, Eq, Hash)]
enum SeenKey {
    Comment(String, String),
    Reply(String, String),
}

/// A surfaceable event: a new open `you` top-level comment, or a new `you`
/// reply on any comment. The reply case is what makes the loop two-way —
/// when the user taps a quick-response button (or types a reply) on one of
/// Claude's comments, the SPA appends a `Reply{author: you}` and this
/// surfaces it so Claude can respond in the same session.
#[derive(Clone)]
enum Surfaced {
    Comment(serde_json::Value),
    Reply {
        parent: serde_json::Value,
        reply: serde_json::Value,
    },
}

/// Dedupe key for a surfaced item (pure).
fn seen_key(aid: &str, item: &Surfaced) -> SeenKey {
    match item {
        Surfaced::Comment(c) => SeenKey::Comment(aid.to_string(), comment_id(c).to_string()),
        Surfaced::Reply { reply, .. } => {
            SeenKey::Reply(aid.to_string(), reply_id(reply).to_string())
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    kb: Option<&str>,
    path: &str,
    json: bool,
    once: bool,
    timeout_secs: Option<u64>,
    backlog: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let base = daemon
        .unwrap_or("http://127.0.0.1:4000")
        .trim_end_matches('/');
    let kb_name = crate::http::resolve_default_kb(kb, daemon, bearer).await?;

    // 1. Resolve scope → { artifact_id: source_relative }.
    let scope = resolve_scope(base, &kb_name, path, bearer).await?;
    if scope.is_empty() {
        anyhow::bail!("--path {path:?} matched no artifact or folder in {kb_name}");
    }
    eprintln!(
        "[kb comments watch] watching {} artifact(s) in {kb_name} (scope: {path})",
        scope.len()
    );

    // 2. Seed the seen-set so the existing backlog isn't re-handled —
    //    unless `--backlog`, in which case emit it first.
    let mut seen: HashSet<SeenKey> = HashSet::new();
    let mut emitted_any = false;
    for (aid, rel) in &scope {
        for item in fetch_surfaceable(base, &kb_name, aid, bearer).await? {
            seen.insert(seen_key(aid, &item));
            if backlog {
                emit(&kb_name, aid, rel, &item, json);
                emitted_any = true;
            }
        }
    }
    if once && emitted_any {
        return Ok(());
    }

    // 3. Subscribe + reconnect loop (mirrors `kb push`). On each connect
    //    we rescan in-scope reviews so a comment posted during a
    //    reconnect gap is still surfaced.
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    #[cfg(unix)]
    let mut sigterm =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();

    let mut last_id: Option<String> = None;
    let mut step = 0usize;
    let deadline = timeout_secs.map(|s| Instant::now() + Duration::from_secs(s));

    loop {
        let resume = last_id.clone();
        let started = Instant::now();
        let outcome: Result<WatchExit> = tokio::select! {
            biased;
            _ = &mut ctrl_c => {
                eprintln!("[kb comments watch] received SIGINT; exiting");
                return Ok(());
            }
            _ = async {
                #[cfg(unix)]
                { if let Some(s) = sigterm.as_mut() { s.recv().await; } else { std::future::pending::<()>().await; } }
                #[cfg(not(unix))]
                { std::future::pending::<()>().await; }
            } => {
                eprintln!("[kb comments watch] received SIGTERM; exiting");
                return Ok(());
            }
            o = watch_once(
                base, &kb_name, bearer, resume.as_deref(),
                &scope, &mut seen, json, once, deadline, &mut last_id,
            ) => o,
        };
        match outcome {
            Ok(WatchExit::Done) => return Ok(()),
            Ok(WatchExit::StreamClosed) => {
                let uptime = started.elapsed();
                if uptime >= STABLE_SESSION {
                    step = 0;
                    eprintln!("[kb comments watch] stream closed; reconnecting in 1s …");
                } else {
                    let delay = BACKOFF_STEPS[step.min(BACKOFF_STEPS.len() - 1)];
                    eprintln!("[kb comments watch] stream closed; retrying in {delay}s …");
                    step = (step + 1).min(BACKOFF_STEPS.len() - 1);
                }
            }
            Err(e) => {
                let delay = BACKOFF_STEPS[step.min(BACKOFF_STEPS.len() - 1)];
                eprintln!("[kb comments watch] {e}; retrying in {delay}s …");
                step = (step + 1).min(BACKOFF_STEPS.len() - 1);
            }
        }
        let delay = BACKOFF_STEPS[step.min(BACKOFF_STEPS.len() - 1)];
        // Abort backoff if a signal fires, and respect the idle deadline.
        let sleep = tokio::time::sleep(Duration::from_secs(delay));
        tokio::pin!(sleep);
        tokio::select! {
            biased;
            _ = &mut ctrl_c => return Ok(()),
            _ = sleep_until_opt(deadline) => return Ok(()),
            _ = &mut sleep => {}
        }
    }
}

enum WatchExit {
    /// `--once` satisfied or the idle `--timeout` elapsed — exit 0.
    Done,
    /// The SSE stream closed; the caller reconnects.
    StreamClosed,
}

/// One connected session: drain frames, surface in-scope new comments.
/// Returns `Done` when `--once`/timeout is satisfied, `StreamClosed`
/// when the stream ends (caller reconnects).
#[allow(clippy::too_many_arguments)]
async fn watch_once(
    base: &str,
    kb_name: &str,
    bearer: Option<&str>,
    last_event_id: Option<&str>,
    scope: &HashMap<String, String>,
    seen: &mut HashSet<SeenKey>,
    json: bool,
    once: bool,
    deadline: Option<Instant>,
    last_id: &mut Option<String>,
) -> Result<WatchExit> {
    let resp = open_events_stream(base, bearer, last_event_id, "").await?;
    let mut reader = FrameReader::from_response(resp);

    // Rescan the scope on (re)connect to catch anything missed while
    // disconnected. On the very first connect this finds nothing new
    // (seeded above); the seen-set persists across reconnects so old
    // comments/replies are never re-surfaced.
    for (aid, rel) in scope {
        for item in fetch_surfaceable(base, kb_name, aid, bearer).await? {
            if surface_if_new(seen, kb_name, aid, rel, &item, json) && once {
                return Ok(WatchExit::Done);
            }
        }
    }

    loop {
        let frame = tokio::select! {
            biased;
            _ = sleep_until_opt(deadline) => return Ok(WatchExit::Done),
            f = reader.next_frame() => f?,
        };
        let Some(frame) = frame else {
            return Ok(WatchExit::StreamClosed);
        };
        if let Some(id) = &frame.id {
            *last_id = Some(id.clone());
        }
        if frame.event.as_deref() != Some("comments.updated") {
            continue;
        }
        let raw = frame.data.as_deref().unwrap_or("{}");
        let envelope: serde_json::Value = match serde_json::from_str(raw) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[kb comments watch] malformed comments.updated payload: {e}");
                continue;
            }
        };
        // SSE frames wrap the event data in `{ payload: {...}, ts, v }`;
        // the kb/artifact_id fields live under `payload`. Fall back to
        // the top level so a future flattened emit still parses.
        let payload = envelope.get("payload").unwrap_or(&envelope);
        if payload.get("kb").and_then(|v| v.as_str()) != Some(kb_name) {
            continue;
        }
        let Some(aid) = payload.get("artifact_id").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(rel) = scope.get(aid) else {
            continue; // not in scope
        };
        for item in fetch_surfaceable(base, kb_name, aid, bearer).await? {
            if surface_if_new(seen, kb_name, aid, rel, &item, json) && once {
                return Ok(WatchExit::Done);
            }
        }
    }
}

/// Emit a surfaceable item if its key hasn't been seen. Returns `true`
/// when it emitted (i.e. it was new).
fn surface_if_new(
    seen: &mut HashSet<SeenKey>,
    kb_name: &str,
    aid: &str,
    source_rel: &str,
    item: &Surfaced,
    json: bool,
) -> bool {
    let key = seen_key(aid, item);
    if seen.contains(&key) {
        return false;
    }
    seen.insert(key);
    emit(kb_name, aid, source_rel, item, json);
    true
}

/// A future that resolves at `deadline`, or never if `None`.
async fn sleep_until_opt(deadline: Option<Instant>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(d).await,
        None => std::future::pending::<()>().await,
    }
}

/// Resolve `--path` to `{ artifact_id: source_relative }`. Tries the
/// `/lookup` endpoint first (single artifact); on `not_found` falls back
/// to a folder query (`/docs?folder=`), treating the path as a folder.
async fn resolve_scope(
    base: &str,
    kb_name: &str,
    path: &str,
    bearer: Option<&str>,
) -> Result<HashMap<String, String>> {
    let body = crate::http::get_lookup(Some(base), kb_name, path, bearer).await?;
    match classify_lookup(&body) {
        LookupKind::Single => {
            let id = body
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("lookup response missing `id`"))?
                .to_string();
            let rel = body
                .get("source_relative")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Ok(HashMap::from([(id, rel)]))
        }
        LookupKind::Ambiguous => anyhow::bail!(
            "--path {path:?} matched multiple artifacts in {kb_name}; \
             pass a full source-relative path, or a folder"
        ),
        LookupKind::NotFound => {
            // Treat the path as a folder (descendant-inclusive).
            let folder = path.trim_end_matches('/');
            fetch_folder_scope(base, kb_name, folder, bearer).await
        }
    }
}

enum LookupKind {
    Single,
    Ambiguous,
    NotFound,
}

/// Classify a `/lookup` response by its `kind` field (pure; testable).
fn classify_lookup(body: &serde_json::Value) -> LookupKind {
    match body.get("kind").and_then(|v| v.as_str()) {
        Some("exact") | Some("unique_suffix") => LookupKind::Single,
        Some("ambiguous") => LookupKind::Ambiguous,
        _ => LookupKind::NotFound,
    }
}

/// Enumerate every artifact under `folder` via `/docs?folder=…`.
async fn fetch_folder_scope(
    base: &str,
    kb_name: &str,
    folder: &str,
    bearer: Option<&str>,
) -> Result<HashMap<String, String>> {
    let url = format!(
        "{base}/api/kb/{}/docs",
        crate::http::encode_path_segment(kb_name)
    );
    let client = crate::http::client_with_timeout_and_bearer(10, bearer)?;
    let resp = client
        .get(&url)
        .query(&[("folder", folder), ("envelope", "1"), ("limit", "500")])
        .send()
        .await
        .with_context(|| format!("GET {url}?folder={folder}"))?;
    let body: serde_json::Value = resp
        .error_for_status()
        .with_context(|| format!("GET {url}?folder={folder}"))?
        .json()
        .await?;
    Ok(folder_scope_from_docs(&body))
}

/// Extract `{ id: source_relative }` from a `/docs` envelope (pure).
fn folder_scope_from_docs(body: &serde_json::Value) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let docs = body.get("docs").and_then(|v| v.as_array());
    for d in docs.into_iter().flatten() {
        if let Some(id) = d.get("id").and_then(|v| v.as_str()) {
            let rel = d
                .get("source_relative")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            out.insert(id.to_string(), rel);
        }
    }
    out
}

/// GET an artifact's review and return its surfaceable items. A 404 (no
/// review yet) yields an empty vec.
async fn fetch_surfaceable(
    base: &str,
    kb_name: &str,
    artifact_id: &str,
    bearer: Option<&str>,
) -> Result<Vec<Surfaced>> {
    let url = format!(
        "{base}/api/kb/{}/review/{}",
        crate::http::encode_path_segment(kb_name),
        crate::http::encode_path_segment(artifact_id),
    );
    let client = crate::http::client_with_timeout_and_bearer(5, bearer)?;
    let resp = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    if resp.status().as_u16() == 404 {
        return Ok(Vec::new());
    }
    let body: serde_json::Value = resp
        .error_for_status()
        .with_context(|| format!("GET {url}"))?
        .json()
        .await?;
    Ok(surfaceable_items(&body))
}

/// Pull surfaceable items from a review file (pure):
///   - open top-level comments authored by `you`
///   - replies authored by `you` on **any** comment, open or resolved — a
///     resolve-choice click appends a `you` reply *and* flips the comment
///     to resolved, so gating reply surfacing on `open` would hide exactly
///     that reply. Claude's own replies (`author: claude`) are skipped, so
///     the loop stays echo-safe.
fn surfaceable_items(review: &serde_json::Value) -> Vec<Surfaced> {
    let mut out = Vec::new();
    let Some(comments) = review.get("comments").and_then(|v| v.as_array()) else {
        return out;
    };
    for c in comments {
        let author = c.get("author").and_then(|v| v.as_str());
        let status = c.get("status").and_then(|v| v.as_str());
        if author == Some("you") && status == Some("open") {
            out.push(Surfaced::Comment(c.clone()));
        }
        if let Some(replies) = c.get("replies").and_then(|v| v.as_array()) {
            for r in replies {
                if r.get("author").and_then(|v| v.as_str()) == Some("you") {
                    out.push(Surfaced::Reply {
                        parent: c.clone(),
                        reply: r.clone(),
                    });
                }
            }
        }
    }
    out
}

fn comment_id(comment: &serde_json::Value) -> &str {
    comment.get("id").and_then(|v| v.as_str()).unwrap_or("")
}

fn reply_id(reply: &serde_json::Value) -> &str {
    reply.get("id").and_then(|v| v.as_str()).unwrap_or("")
}

/// Print one surfaced item — a JSON line (`--json`) or a human block.
fn emit(kb_name: &str, aid: &str, source_rel: &str, item: &Surfaced, json: bool) {
    if json {
        let line = surfaced_json(kb_name, aid, source_rel, item);
        if let Ok(s) = serde_json::to_string(&line) {
            println!("{s}");
        }
        return;
    }
    let loc = if source_rel.is_empty() {
        aid
    } else {
        source_rel
    };
    match item {
        Surfaced::Comment(c) => {
            let cid = comment_id(c);
            let body = c.get("body").and_then(|v| v.as_str()).unwrap_or("");
            let anchor = anchor_label(c.get("anchor"));
            println!("● new comment {cid} on {loc}  [{anchor}]");
            for line in body.lines() {
                println!("    {line}");
            }
            println!(
                "  → reply:   kb comments reply {cid} --path {loc} --body \"…\"\n  → resolve: kb comments resolve --path {loc} {cid}",
            );
        }
        Surfaced::Reply { parent, reply } => {
            let cid = comment_id(parent);
            let rid = reply_id(reply);
            let body = reply.get("body").and_then(|v| v.as_str()).unwrap_or("");
            let pauthor = parent.get("author").and_then(|v| v.as_str()).unwrap_or("?");
            let pbody = parent.get("body").and_then(|v| v.as_str()).unwrap_or("");
            let ctx: String = pbody.chars().take(60).collect();
            println!("↳ new reply {rid} on comment {cid} ({loc})");
            println!("  (re {pauthor}: {ctx})");
            for line in body.lines() {
                println!("    {line}");
            }
            println!(
                "  → reply:   kb comments reply {cid} --path {loc} --body \"…\"\n  → resolve: kb comments resolve --path {loc} {cid}",
            );
        }
    }
}

/// Build the `--json` line for a surfaced item (pure; testable). Carries a
/// `kind` discriminator; replies add `reply_id` + `in_reply_to` context.
fn surfaced_json(kb_name: &str, aid: &str, source_rel: &str, item: &Surfaced) -> serde_json::Value {
    match item {
        Surfaced::Comment(c) => serde_json::json!({
            "kb": kb_name,
            "artifact_id": aid,
            "source_relative": source_rel,
            "kind": "comment",
            "comment_id": c.get("id").cloned().unwrap_or(serde_json::Value::Null),
            "anchor": c.get("anchor").cloned().unwrap_or(serde_json::Value::Null),
            "body": c.get("body").cloned().unwrap_or(serde_json::Value::Null),
            "created_at": c.get("createdAt").cloned().unwrap_or(serde_json::Value::Null),
        }),
        Surfaced::Reply { parent, reply } => serde_json::json!({
            "kb": kb_name,
            "artifact_id": aid,
            "source_relative": source_rel,
            "kind": "reply",
            "comment_id": parent.get("id").cloned().unwrap_or(serde_json::Value::Null),
            "reply_id": reply.get("id").cloned().unwrap_or(serde_json::Value::Null),
            "anchor": parent.get("anchor").cloned().unwrap_or(serde_json::Value::Null),
            "body": reply.get("body").cloned().unwrap_or(serde_json::Value::Null),
            "created_at": reply.get("createdAt").cloned().unwrap_or(serde_json::Value::Null),
            "in_reply_to": {
                "author": parent.get("author").cloned().unwrap_or(serde_json::Value::Null),
                "body": parent.get("body").cloned().unwrap_or(serde_json::Value::Null),
            },
        }),
    }
}

/// Compact one-word-ish anchor label for the human block.
fn anchor_label(anchor: Option<&serde_json::Value>) -> String {
    let Some(a) = anchor else {
        return "?".to_string();
    };
    match a.get("kind").and_then(|v| v.as_str()) {
        Some("file") => "file".to_string(),
        Some("chapter") => format!(
            "chapter:{}",
            a.get("path").and_then(|v| v.as_str()).unwrap_or("?")
        ),
        Some("section") => format!(
            "section:{}",
            a.get("id").and_then(|v| v.as_str()).unwrap_or("?")
        ),
        Some("selection") => "selection".to_string(),
        _ => "?".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn classify_lookup_maps_kinds() {
        assert!(matches!(
            classify_lookup(&json!({"kind":"exact"})),
            LookupKind::Single
        ));
        assert!(matches!(
            classify_lookup(&json!({"kind":"unique_suffix"})),
            LookupKind::Single
        ));
        assert!(matches!(
            classify_lookup(&json!({"kind":"ambiguous"})),
            LookupKind::Ambiguous
        ));
        assert!(matches!(
            classify_lookup(&json!({"kind":"not_found"})),
            LookupKind::NotFound
        ));
    }

    #[test]
    fn folder_scope_collects_id_and_source_relative() {
        let body = json!({
            "docs": [
                {"id": "aaa111", "source_relative": "ideas/foo/a.html"},
                {"id": "bbb222", "source_relative": "ideas/foo/b.html"},
            ],
            "total": 2
        });
        let scope = folder_scope_from_docs(&body);
        assert_eq!(scope.len(), 2);
        assert_eq!(scope.get("aaa111").unwrap(), "ideas/foo/a.html");
        assert_eq!(scope.get("bbb222").unwrap(), "ideas/foo/b.html");
    }

    #[test]
    fn surfaceable_items_picks_open_you_comments_and_you_replies() {
        let review = json!({
            "comments": [
                // open you comment → surfaced as a Comment
                {"id": "c_1", "status": "open", "author": "you", "body": "x", "replies": []},
                // resolved you comment → NOT surfaced as a comment, but a
                // you reply on it IS (the resolve-choice case).
                {"id": "c_2", "status": "resolved", "author": "you", "body": "y",
                 "replies": [{"id": "r_1", "author": "you", "body": "yes"}]},
                // claude comment with a you reply + a claude reply.
                {"id": "c_3", "status": "open", "author": "claude", "body": "z",
                 "replies": [
                    {"id": "r_2", "author": "you", "body": "ok"},
                    {"id": "r_3", "author": "claude", "body": "done"}
                 ]},
            ]
        });
        let items = surfaceable_items(&review);
        let mut comments = vec![];
        let mut replies = vec![];
        for it in &items {
            match it {
                Surfaced::Comment(c) => comments.push(comment_id(c).to_string()),
                Surfaced::Reply { reply, .. } => replies.push(reply_id(reply).to_string()),
            }
        }
        assert_eq!(comments, vec!["c_1"]); // only the open you comment
        assert!(replies.contains(&"r_1".to_string())); // you reply on resolved
        assert!(replies.contains(&"r_2".to_string())); // you reply on claude
        assert!(!replies.contains(&"r_3".to_string())); // claude reply skipped (echo-safe)
        assert_eq!(replies.len(), 2);
    }

    #[test]
    fn surface_if_new_dedupes_comments_and_replies_distinctly() {
        let mut seen = HashSet::new();
        let c = Surfaced::Comment(json!({"id": "c_1", "body": "x"}));
        assert!(surface_if_new(
            &mut seen, "smoke", "abc123", "a.html", &c, true
        ));
        // Second time → already seen → no emit.
        assert!(!surface_if_new(
            &mut seen, "smoke", "abc123", "a.html", &c, true
        ));
        // Same comment id under a *different* artifact is distinct.
        assert!(surface_if_new(
            &mut seen, "smoke", "def456", "b.html", &c, true
        ));
        // A reply keys on its own id, independent of the comment key.
        let r = Surfaced::Reply {
            parent: json!({"id": "c_1", "author": "claude", "body": "q"}),
            reply: json!({"id": "r_1", "author": "you", "body": "yes"}),
        };
        assert!(surface_if_new(
            &mut seen, "smoke", "abc123", "a.html", &r, true
        ));
        assert!(!surface_if_new(
            &mut seen, "smoke", "abc123", "a.html", &r, true
        ));
    }

    #[test]
    fn surfaced_json_comment_and_reply_shapes() {
        let c = Surfaced::Comment(json!({
            "id": "c_1",
            "anchor": {"kind": "section", "id": "intro"},
            "body": "fix the typo",
            "createdAt": "2026-05-20T10:00:00Z"
        }));
        let line = surfaced_json("smoke", "abc123", "ideas/x.html", &c);
        assert_eq!(line["kind"], "comment");
        assert_eq!(line["comment_id"], "c_1");
        assert_eq!(line["anchor"]["kind"], "section");
        assert_eq!(line["body"], "fix the typo");
        assert_eq!(line["created_at"], "2026-05-20T10:00:00Z");

        let r = Surfaced::Reply {
            parent: json!({
                "id": "c_1", "author": "claude", "body": "want a legend?",
                "anchor": {"kind": "file"}
            }),
            reply: json!({
                "id": "r_9", "author": "you", "body": "yes please",
                "createdAt": "2026-05-20T11:00:00Z"
            }),
        };
        let line = surfaced_json("smoke", "abc123", "ideas/x.html", &r);
        assert_eq!(line["kind"], "reply");
        assert_eq!(line["comment_id"], "c_1");
        assert_eq!(line["reply_id"], "r_9");
        assert_eq!(line["body"], "yes please");
        assert_eq!(line["created_at"], "2026-05-20T11:00:00Z");
        assert_eq!(line["in_reply_to"]["author"], "claude");
        assert_eq!(line["in_reply_to"]["body"], "want a legend?");
    }

    #[test]
    fn anchor_label_renders_kinds() {
        assert_eq!(anchor_label(Some(&json!({"kind": "file"}))), "file");
        assert_eq!(
            anchor_label(Some(&json!({"kind": "section", "id": "intro"}))),
            "section:intro"
        );
        assert_eq!(
            anchor_label(Some(&json!({"kind": "chapter", "path": "A > B"}))),
            "chapter:A > B"
        );
        assert_eq!(anchor_label(None), "?");
    }
}
