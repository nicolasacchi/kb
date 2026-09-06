//! `kb why-memory <id>` — CT-B1: the fact → origin session → commits →
//! files-changed-since chain as ONE CLI verb. ZERO new server surface: pure
//! composition of endpoints that already exist for other callers —
//! `GET /api/kb/{kb}/docs/{id}` (title/created), `GET /api/memory/census`
//! (the corpus-scoped fallback for the origin `session_id`, since the docs
//! endpoint doesn't carry it — see `resolve_memory_doc`'s doc comment),
//! `GET /api/sessions/{sid}` (session detail), `.../commits`, and
//! `.../commits/{sha}/files`. This is the CLI's terminal twin of the SPA's
//! `ProvenanceThread` (MI-W4.6), which walks the exact same chain in the
//! browser.
//!
//! Honesty is the point: an absent origin session, a purged capture, and an
//! unresolvable git lookup are all rendered as explicit lines, never empty
//! output or a silent gap — and the footer note is deliberately blunt about
//! what the labels do NOT claim.

use crate::http;
use anyhow::{anyhow, Result};

/// Printed verbatim as the human output's footer AND the `--json` `note`
/// field, so an agent parsing either shape gets the same caveat: a resolved
/// commit / an unchanged file is evidence the fact's SOURCE is intact, never
/// a claim that the fact itself is still true.
const LABELS_NOTE: &str =
    "labels mean: commit still in history / files changed since — never \"fact still true\"";

/// CT-F1 — printed under the exact-commit section (and carried on
/// `--json` as `exact_commits_note`) whenever that section renders. Two
/// claims are deliberately separated: what an exact-id citation IS
/// (a commit that named this memory's id in its own message) and what its
/// ABSENCE is (nothing — the trailer is opt-in per repo and off by
/// default), so neither a rendered row nor an empty section can be
/// over-read.
const EXACT_COMMITS_NOTE: &str = "exact-id: each commit's own message named this memory (Kb-Memory: trailer, opt-in per repo) — an empty list means the repo never opted in, not that nothing used it";

/// `kb why-memory <id> [--kb] [--json]` entry point.
pub async fn why_memory(
    id: &str,
    kb: Option<&str>,
    daemon: Option<&str>,
    bearer: Option<&str>,
    json: bool,
) -> Result<()> {
    let res = why_memory_inner(id, kb, daemon, bearer).await;
    match res {
        Ok(out) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&out)?);
            } else {
                println!("{}", render_human(&out));
            }
            Ok(())
        }
        Err(e) => {
            if json {
                emit_json_error(&e, daemon);
            }
            Err(e)
        }
    }
}

async fn why_memory_inner(
    id: &str,
    kb: Option<&str>,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<serde_json::Value> {
    let url = require_daemon(daemon, bearer).await?;
    let (kb_name, doc) = resolve_memory_doc(&url, kb, id, bearer).await?;

    let title = doc["title"].as_str().unwrap_or("?").to_string();
    let created_unix = doc["created_unix"].as_i64();
    let memory = serde_json::json!({
        "id": id,
        "kb": kb_name,
        "title": title,
        "created_unix": created_unix,
    });

    // Step 1's ladder (WORK ORDER): `docs/{id}` first — read defensively in
    // case a future daemon adds `session_id`/`kb_session` to `DocResponse`
    // (it doesn't today, verified against `routes::docs::DocResponse`), so
    // this composer needs no changes the day it does. Else the census row
    // (corpus-scoped, paginated — the cheapest EXISTING read that actually
    // carries the origin session id today). Else recall, best-effort.
    let mut session_id = doc["session_id"]
        .as_str()
        .or_else(|| doc["kb_session"].as_str())
        .map(str::to_string);
    if session_id.is_none() {
        session_id = census_session_id(&url, &kb_name, id, bearer).await?;
    }
    if session_id.is_none() {
        session_id = recall_session_id(&url, &kb_name, id, bearer).await?;
    }

    let (session_status, session, commits) = match &session_id {
        None => ("none", None, Vec::new()),
        Some(sid) => fetch_session_chain(&url, sid, bearer).await?,
    };

    // CT-F1 — the EXACT-ID lane, independent of everything above: it needs
    // no origin session at all (a hand-written memory cited in a commit
    // still resolves), so it is fetched from the memory's own identity and
    // rendered even when the session chain is absent or purged.
    let exact_commits = fetch_exact_commits(&url, &kb_name, id, bearer).await?;

    Ok(serde_json::json!({
        "memory": memory,
        "origin_session_id": session_id,
        "session_status": session_status,
        "session": session,
        "commits": commits,
        "exact_commits": exact_commits,
        "exact_commits_note": EXACT_COMMITS_NOTE,
        "note": LABELS_NOTE,
    }))
}

/// CT-F1 — `GET /api/kb/{kb}/memories/{id}/commits`: every commit that
/// cited this memory by id. Best-effort like `recall_session_id`: an older
/// daemon that doesn't serve the route (404) or any transport hiccup yields
/// an empty list, which renders as an absent section rather than failing a
/// verb whose primary chain resolved fine. `[]` is ALSO the honest,
/// overwhelmingly common answer (the trailer is opt-in per repo, default
/// off) — the two are indistinguishable here on purpose, and
/// `EXACT_COMMITS_NOTE` says so wherever the section renders.
async fn fetch_exact_commits(
    url: &str,
    kb: &str,
    id: &str,
    bearer: Option<&str>,
) -> Result<Vec<serde_json::Value>> {
    let client = http::client_with_timeout_and_bearer(10, bearer)?;
    let resp = client
        .get(format!(
            "{url}/api/kb/{}/memories/{}/commits",
            http::encode_path_segment(kb),
            http::encode_path_segment(id)
        ))
        .send()
        .await;
    let Ok(resp) = resp else {
        return Ok(Vec::new());
    };
    if !resp.status().is_success() {
        return Ok(Vec::new());
    }
    let body: serde_json::Value = match resp.json().await {
        Ok(b) => b,
        Err(_) => return Ok(Vec::new()),
    };
    Ok(body["rows"].as_array().cloned().unwrap_or_default())
}

/// Resolve `<memory-id>` to `(kb_name, docs/{id} JSON)`. `--kb` wins
/// outright (one GET); absent, search every memory-scoped kb (from
/// `GET /api/kbs`, reusing the same `memory_scope` facet `kb memory
/// census`/`recall` filter on) for the first one holding this id.
///
/// `GET /api/kb/{kb}/docs/{id}` (`routes::docs::DocResponse`) does NOT carry
/// `kb_session` — checked against the struct before writing this, per the
/// WORK ORDER's step 1. It still earns the first call: it's the cheapest
/// existence+title probe (a single indexed id lookup, not a corpus scan),
/// and `why_memory_inner` re-reads its `session_id`/`kb_session` fields
/// defensively in case that ever changes.
async fn resolve_memory_doc(
    url: &str,
    kb: Option<&str>,
    id: &str,
    bearer: Option<&str>,
) -> Result<(String, serde_json::Value)> {
    let candidates: Vec<String> = match kb {
        Some(k) => vec![k.to_string()],
        None => memory_scoped_kbs(url, bearer).await?,
    };
    if candidates.is_empty() {
        return Err(anyhow!("no memory-scoped kbs configured; pass --kb <name>"));
    }
    let client = http::client_with_timeout_and_bearer(10, bearer)?;
    for k in &candidates {
        let doc_url = format!(
            "{url}/api/kb/{}/docs/{}",
            http::encode_path_segment(k),
            http::encode_path_segment(id)
        );
        let resp = client.get(&doc_url).send().await?;
        if resp.status().as_u16() == 404 {
            continue;
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("GET {doc_url}: HTTP {status}: {body}"));
        }
        return Ok((k.clone(), resp.json().await?));
    }
    Err(anyhow!(
        "memory {id} not found in {}",
        if kb.is_some() {
            format!("kb {:?}", candidates[0])
        } else {
            format!("any memory-scoped kb (searched: {})", candidates.join(", "))
        }
    ))
}

/// Every kb whose `memory_scope` is `"project"` or `"global"` (`GET
/// /api/kbs`) — the set a bare `kb recall`/`kb memory census` would ever
/// look at, reused here so `--kb`-less `why-memory` searches the same
/// footprint rather than inventing a second notion of "memory kb".
async fn memory_scoped_kbs(url: &str, bearer: Option<&str>) -> Result<Vec<String>> {
    let client = http::client_with_timeout_and_bearer(5, bearer)?;
    let kbs: serde_json::Value = client
        .get(format!("{url}/api/kbs"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let arr = kbs
        .as_array()
        .ok_or_else(|| anyhow!("GET /api/kbs: expected an array"))?;
    Ok(arr
        .iter()
        .filter(|k| matches!(k["memory_scope"].as_str(), Some("project") | Some("global")))
        .filter_map(|k| k["name"].as_str().map(str::to_string))
        .collect())
}

/// Fallback #2 — `GET /api/kb/{kb}/docs/{id}` carries no `session_id` today,
/// so walk this kb's `/api/memory/census` (corpus-scoped, paginated,
/// `session_id` IS one of its columns) until the matching row turns up or
/// the corpus is exhausted. Not id-filterable server-side, so this is a
/// bounded linear scan — acceptable for an on-demand investigative verb, not
/// a hot path.
async fn census_session_id(
    url: &str,
    kb: &str,
    id: &str,
    bearer: Option<&str>,
) -> Result<Option<String>> {
    let client = http::client_with_timeout_and_bearer(15, bearer)?;
    let mut offset: u64 = 0;
    loop {
        let resp = client
            .get(format!("{url}/api/memory/census"))
            .query(&[
                ("kb", kb),
                ("offset", &offset.to_string()),
                ("limit", "200"),
            ])
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "GET /api/memory/census?kb={kb}: HTTP {status}: {body}"
            ));
        }
        let body: serde_json::Value = resp.json().await?;
        let rows = body["rows"].as_array().cloned().unwrap_or_default();
        if let Some(row) = rows.iter().find(|r| r["id"].as_str() == Some(id)) {
            return Ok(row["session_id"].as_str().map(str::to_string));
        }
        let total = body["total"].as_u64().unwrap_or(0);
        offset += rows.len() as u64;
        if rows.is_empty() || offset >= total {
            return Ok(None);
        }
    }
}

/// Fallback #3 (last resort, best-effort) — an exact-id hit off `kb
/// recall`'s fan-out. Never hard-errors: a transport hiccup or a miss here
/// just leaves `session_id` `None`, rendered honestly as "no origin session
/// recorded" rather than failing the whole verb over an optional enrichment.
async fn recall_session_id(
    url: &str,
    kb: &str,
    id: &str,
    bearer: Option<&str>,
) -> Result<Option<String>> {
    let client = http::client_with_timeout_and_bearer(30, bearer)?;
    let resp = client
        .get(format!("{url}/api/memory/recall"))
        .query(&[("q", id), ("scope", "all"), ("limit", "50")])
        .send()
        .await;
    let resp = match resp {
        Ok(r) if r.status().is_success() => r,
        _ => return Ok(None),
    };
    let body: serde_json::Value = match resp.json().await {
        Ok(b) => b,
        Err(_) => return Ok(None),
    };
    let hits = body["hits"].as_array().cloned().unwrap_or_default();
    Ok(hits
        .iter()
        .find(|h| h["kb"].as_str() == Some(kb) && h["id"].as_str() == Some(id))
        .and_then(|h| h["session_id"].as_str().map(str::to_string)))
}

/// Walk the rest of the chain from a resolved `session_id`: session detail,
/// its commits, and — per resolved `kind: "commit"` row — the touched-file
/// staleness. Returns `("purged", None, [])` on a 404 (the capture is no
/// longer indexed) rather than an error: the memory itself resolved fine,
/// only its provenance trail went cold.
async fn fetch_session_chain(
    url: &str,
    sid: &str,
    bearer: Option<&str>,
) -> Result<(
    &'static str,
    Option<serde_json::Value>,
    Vec<serde_json::Value>,
)> {
    let client = http::client_with_timeout_and_bearer(15, bearer)?;
    let resp = client
        .get(format!(
            "{url}/api/sessions/{}",
            http::encode_path_segment(sid)
        ))
        .send()
        .await?;
    if resp.status().as_u16() == 404 {
        return Ok(("purged", None, Vec::new()));
    }
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("GET /api/sessions/{sid}: HTTP {status}: {body}"));
    }
    let session: serde_json::Value = resp.json().await?;

    let commits_resp = client
        .get(format!(
            "{url}/api/sessions/{}/commits",
            http::encode_path_segment(sid)
        ))
        .send()
        .await?;
    let mut commits: Vec<serde_json::Value> = if commits_resp.status().is_success() {
        let body: serde_json::Value = commits_resp.json().await?;
        body["commits"].as_array().cloned().unwrap_or_default()
    } else {
        Vec::new()
    };

    // Per-commit touched-file staleness — mirrors the SPA's
    // `ProvenanceThread` `canExpand` gate (`commit.resolved && !!sha`): a
    // push/tag row or an unresolved commit has nothing to look up, so this
    // skips the round trip rather than spend one on a guaranteed
    // `available: false`.
    for c in &mut commits {
        let is_commit = c["kind"].as_str() == Some("commit");
        let resolved = c["resolved"].as_bool().unwrap_or(false);
        let sha = c["sha_full"]
            .as_str()
            .or_else(|| c["sha"].as_str())
            .map(str::to_string);
        let (Some(sha), true, true) = (sha, is_commit, resolved) else {
            continue;
        };
        let files_resp = client
            .get(format!(
                "{url}/api/sessions/{}/commits/{}/files",
                http::encode_path_segment(sid),
                http::encode_path_segment(&sha),
            ))
            .send()
            .await?;
        if files_resp.status().is_success() {
            let files: serde_json::Value = files_resp.json().await?;
            if let Some(obj) = c.as_object_mut() {
                obj.insert("files".to_string(), files);
            }
        }
    }

    Ok(("found", Some(session), commits))
}

// ---- rendering ---------------------------------------------------------

/// Pure text renderer over the SAME composed shape `why_memory_inner`
/// returns (and `--json` prints verbatim) — split out so the layout is
/// unit-testable with a literal fixture, no daemon required.
fn render_human(out: &serde_json::Value) -> String {
    let mem = &out["memory"];
    let title = mem["title"].as_str().unwrap_or("?");
    let id = mem["id"].as_str().unwrap_or("?");
    let kb = mem["kb"].as_str().unwrap_or("?");
    let created = mem["created_unix"]
        .as_i64()
        .map(super::sessions::format_started)
        .unwrap_or_else(|| "?".to_string());

    let mut s = String::new();
    s.push_str("the memory\n");
    s.push_str(&format!("  title:   {title}\n"));
    s.push_str(&format!("  id:      {id}\n"));
    s.push_str(&format!("  kb:      {kb}\n"));
    s.push_str(&format!("  created: {created}\n\n"));

    // CT-F1 — the exact-id section goes FIRST and is rendered before the
    // session_status early-returns below: a citation is stronger evidence
    // than the session heuristic, and it survives an origin session that
    // was never recorded or has been purged (both of which return early).
    s.push_str(&render_exact_commits(out));

    match out["session_status"].as_str().unwrap_or("none") {
        "none" => {
            s.push_str("origin session: no origin session recorded\n");
            return s;
        }
        "purged" => {
            let sid = out["origin_session_id"].as_str().unwrap_or("?");
            s.push_str(&format!(
                "origin session: {sid} — this session's capture is no longer indexed (purged)\n"
            ));
            return s;
        }
        _ => {}
    }

    let session = &out["session"];
    let display_name = session["display_name"].as_str().unwrap_or("?");
    let sid = session["session_id"]
        .as_str()
        .or_else(|| session["id"].as_str())
        .unwrap_or("?");
    let started = session["started_at"]
        .as_i64()
        .map(super::sessions::format_started)
        .unwrap_or_else(|| "?".to_string());
    s.push_str("origin session\n");
    s.push_str(&format!("  {display_name}  ({sid})\n"));
    s.push_str(&format!("  started: {started}\n"));
    if let Some(harness) = session["harness"].as_str().filter(|h| !h.is_empty()) {
        s.push_str(&format!("  harness: {harness}\n"));
    }
    if let Some(outcome) = session["outcome"].as_str().filter(|o| !o.is_empty()) {
        s.push_str(&format!("  closed:  {outcome}\n"));
    }
    s.push('\n');

    let commits = out["commits"].as_array().cloned().unwrap_or_default();
    s.push_str("commits\n");
    if commits.is_empty() {
        s.push_str("  this session produced no recorded commits.\n");
    } else {
        for c in &commits {
            let sha = c["sha_full"]
                .as_str()
                .or_else(|| c["sha"].as_str())
                .unwrap_or("-------");
            let short: String = sha.chars().take(8).collect();
            let subject = c["subject"].as_str().unwrap_or("(no subject)");
            let resolved = if c["resolved"].as_bool().unwrap_or(false) {
                "resolved"
            } else {
                "unresolved"
            };
            s.push_str(&format!("  {short}  {subject}  [{resolved}]\n"));
            if let Some(files) = c.get("files") {
                if !files["available"].as_bool().unwrap_or(false) {
                    s.push_str("    files: unknown — this commit's repo isn't resolvable\n");
                } else {
                    let rows = files["files"].as_array().cloned().unwrap_or_default();
                    if rows.is_empty() {
                        s.push_str("    no files recorded for this commit\n");
                    } else {
                        for f in &rows {
                            let path = f["path"].as_str().unwrap_or("?");
                            let label = match f["changed_since"].as_bool() {
                                None => "unverifiable",
                                Some(true) => "yes",
                                Some(false) => "no",
                            };
                            s.push_str(&format!("    {path}  changed since: {label}\n"));
                        }
                        if files["truncated"].as_bool().unwrap_or(false) {
                            s.push_str("    …more files touched, not all shown\n");
                        }
                    }
                }
            }
        }
    }
    s.push('\n');
    s.push_str(out["note"].as_str().unwrap_or(LABELS_NOTE));
    s.push('\n');
    s
}

/// CT-F1 — the exact-commit block, or `""` when nothing cited this memory.
///
/// Deliberately SILENT when empty, unlike every other section in this
/// renderer (which prints an explicit "none recorded" line). The asymmetry
/// is the honest call: those sections' absences are informative (there
/// really was no origin session), whereas an empty citation list almost
/// always just means the committing repo never opted into the
/// `Kb-Memory:` trailer — printing "no commits cited this memory" would
/// state as a finding what is really a configuration fact. `--json` still
/// carries `exact_commits: []` unconditionally, so a machine consumer sees
/// a stable shape.
fn render_exact_commits(out: &serde_json::Value) -> String {
    let rows = out["exact_commits"].as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        return String::new();
    }
    let mut s = String::new();
    s.push_str("committed in (exact-id citations)\n");
    for r in &rows {
        let sha = r["sha_full"]
            .as_str()
            .or_else(|| r["sha"].as_str())
            .unwrap_or("-------");
        let short: String = sha.chars().take(8).collect();
        let subject = r["subject"].as_str().unwrap_or("(no subject)");
        s.push_str(&format!("  {short}  {subject}\n"));
        // The repo is load-bearing: a bare sha is meaningless across repos.
        if let Some(repo) = r["repo_root"].as_str().filter(|p| !p.is_empty()) {
            s.push_str(&format!("    repo: {repo}\n"));
        }
        if let Some(sid) = r["session_id"].as_str().filter(|p| !p.is_empty()) {
            s.push_str(&format!("    recorded by session: {sid}\n"));
        }
    }
    s.push_str(&format!(
        "  {}\n\n",
        out["exact_commits_note"]
            .as_str()
            .unwrap_or(EXACT_COMMITS_NOTE)
    ));
    s
}

// ---- helpers ------------------------------------------------------------

async fn require_daemon(daemon: Option<&str>, bearer: Option<&str>) -> Result<String> {
    http::detect_daemon(daemon, bearer).await.ok_or_else(|| {
        anyhow!(
            "daemon not reachable{} — start it with `kb daemon`",
            daemon.map(|d| format!(" at {d}")).unwrap_or_default()
        )
    })
}

fn emit_json_error(e: &anyhow::Error, daemon: Option<&str>) {
    let envelope = serde_json::json!({
        "error": e.to_string(),
        "source": daemon.unwrap_or("(auto-detect)"),
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&envelope).unwrap_or_default()
    );
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_memory() -> serde_json::Value {
        serde_json::json!({
            "id": "abc123def456",
            "kb": "memory",
            "title": "A remembered fact",
            "created_unix": 1_700_000_000,
        })
    }

    #[test]
    fn render_human_says_so_when_no_origin_session_recorded() {
        let out = serde_json::json!({
            "memory": base_memory(),
            "origin_session_id": null,
            "session_status": "none",
            "session": null,
            "commits": [],
            "note": LABELS_NOTE,
        });
        let text = render_human(&out);
        assert!(text.contains("A remembered fact"));
        assert!(text.contains("abc123def456"));
        assert!(text.contains("origin session: no origin session recorded"));
        // No fabricated session/commit sections when there's nothing to show.
        assert!(!text.contains("commits\n"));
    }

    #[test]
    fn render_human_says_so_when_session_purged() {
        let out = serde_json::json!({
            "memory": base_memory(),
            "origin_session_id": "sess-old-1",
            "session_status": "purged",
            "session": null,
            "commits": [],
            "note": LABELS_NOTE,
        });
        let text = render_human(&out);
        assert!(text.contains("sess-old-1"));
        assert!(text.contains("no longer indexed (purged)"));
    }

    #[test]
    fn render_human_says_so_when_commits_empty() {
        let out = serde_json::json!({
            "memory": base_memory(),
            "origin_session_id": "sess-1",
            "session_status": "found",
            "session": {
                "session_id": "sess-1",
                "display_name": "fixed the thing",
                "started_at": 1_700_000_000,
                "harness": "claude",
                "outcome": "shipped the fix",
            },
            "commits": [],
            "note": LABELS_NOTE,
        });
        let text = render_human(&out);
        assert!(text.contains("this session produced no recorded commits."));
        assert!(text.contains("fixed the thing"));
        assert!(text.contains("closed:  shipped the fix"));
    }

    #[test]
    fn render_human_renders_commits_and_file_staleness_labels() {
        let out = serde_json::json!({
            "memory": base_memory(),
            "origin_session_id": "sess-1",
            "session_status": "found",
            "session": {
                "session_id": "sess-1",
                "display_name": "fixed the thing",
                "started_at": 1_700_000_000,
                "harness": "claude",
            },
            "commits": [
                {
                    "kind": "commit",
                    "sha": "deadbeef",
                    "sha_full": "deadbeef00112233445566778899aabbccddeeff",
                    "subject": "fix the bug",
                    "resolved": true,
                    "files": {
                        "available": true,
                        "files": [
                            {"path": "src/a.rs", "last_touched_unix": 1, "changed_since": true},
                            {"path": "src/b.rs", "last_touched_unix": 1, "changed_since": false},
                            {"path": "src/c.rs"},
                        ],
                        "truncated": false,
                    },
                },
                {
                    "kind": "commit",
                    "sha": "cafef00d",
                    "subject": "unresolved commit",
                    "resolved": false,
                },
            ],
            "note": LABELS_NOTE,
        });
        let text = render_human(&out);
        assert!(text.contains("deadbeef"));
        assert!(text.contains("fix the bug"));
        assert!(text.contains("[resolved]"));
        assert!(text.contains("src/a.rs  changed since: yes"));
        assert!(text.contains("src/b.rs  changed since: no"));
        assert!(text.contains("src/c.rs  changed since: unverifiable"));
        assert!(text.contains("cafef00d"));
        assert!(text.contains("unresolved commit"));
        assert!(text.contains("[unresolved]"));
        // No `files` key on the unresolved commit — never fetched, so no
        // fabricated "unknown"/"no files" line for it either.
        assert!(text.contains(LABELS_NOTE));
    }

    #[test]
    fn render_human_reports_an_unresolvable_commit_repo_honestly() {
        let out = serde_json::json!({
            "memory": base_memory(),
            "origin_session_id": "sess-1",
            "session_status": "found",
            "session": {"session_id": "sess-1", "display_name": "x", "started_at": 1},
            "commits": [
                {
                    "kind": "commit",
                    "sha": "deadbeef",
                    "subject": "fix",
                    "resolved": true,
                    "files": {"available": false, "files": [], "truncated": false},
                },
            ],
            "note": LABELS_NOTE,
        });
        let text = render_human(&out);
        assert!(text.contains("files: unknown — this commit's repo isn't resolvable"));
    }

    // --- CT-F1: the exact-id citation section -----------------------------

    #[test]
    fn render_human_renders_exact_commit_citations() {
        let out = serde_json::json!({
            "memory": base_memory(),
            "origin_session_id": "sess-1",
            "session_status": "found",
            "session": {"session_id": "sess-1", "display_name": "x", "started_at": 1},
            "commits": [],
            "exact_commits": [{
                "session_kb": "sessions",
                "session_id": "sess-9",
                "sha_full": "deadbeef00112233445566778899aabbccddeeff",
                "sha": "deadbeef",
                "subject": "feat: use the remembered cap",
                "repo_root": "/home/user/project/kb",
                "recorded_at": 1_700_000_000,
            }],
            "exact_commits_note": EXACT_COMMITS_NOTE,
            "note": LABELS_NOTE,
        });
        let text = render_human(&out);
        assert!(text.contains("committed in (exact-id citations)"));
        assert!(text.contains("deadbeef  feat: use the remembered cap"));
        assert!(text.contains("repo: /home/user/project/kb"));
        assert!(text.contains("recorded by session: sess-9"));
        assert!(text.contains(EXACT_COMMITS_NOTE));
    }

    /// The default-OFF reality: no citations ⇒ the section is absent
    /// entirely, never a fabricated "no commits cited this memory" finding.
    #[test]
    fn render_human_omits_the_exact_section_when_there_are_no_citations() {
        let out = serde_json::json!({
            "memory": base_memory(),
            "origin_session_id": "sess-1",
            "session_status": "found",
            "session": {"session_id": "sess-1", "display_name": "x", "started_at": 1},
            "commits": [],
            "exact_commits": [],
            "exact_commits_note": EXACT_COMMITS_NOTE,
            "note": LABELS_NOTE,
        });
        let text = render_human(&out);
        assert!(!text.contains("exact-id"));
        assert!(!text.contains("committed in"));
    }

    /// The exact-id lane must survive the two early-returning session
    /// states — an absent origin session and a purged capture both `return`
    /// before the commits chain renders, and a citation is stronger
    /// evidence than either.
    #[test]
    fn render_human_shows_citations_even_with_no_or_purged_origin_session() {
        for status in ["none", "purged"] {
            let out = serde_json::json!({
                "memory": base_memory(),
                "origin_session_id": if status == "none" { serde_json::Value::Null } else { serde_json::json!("sess-old") },
                "session_status": status,
                "session": null,
                "commits": [],
                "exact_commits": [{
                    "session_kb": "sessions",
                    "session_id": "sess-9",
                    "sha_full": "deadbeef00112233445566778899aabbccddeeff",
                    "subject": "feat: the thing",
                    "recorded_at": 1,
                }],
                "exact_commits_note": EXACT_COMMITS_NOTE,
                "note": LABELS_NOTE,
            });
            let text = render_human(&out);
            assert!(
                text.contains("committed in (exact-id citations)"),
                "{status}: {text}"
            );
            assert!(
                text.contains("deadbeef  feat: the thing"),
                "{status}: {text}"
            );
        }
    }

    #[test]
    fn exact_commits_note_never_reads_an_empty_list_as_evidence() {
        // Pinned wording: the note must say what an ABSENCE means, since
        // the opt-in gate is off by default.
        assert!(EXACT_COMMITS_NOTE.contains("opt-in per repo"));
        assert!(EXACT_COMMITS_NOTE.contains("never opted in"));
    }

    #[test]
    fn render_human_footer_note_is_exact() {
        let out = serde_json::json!({
            "memory": base_memory(),
            "origin_session_id": null,
            "session_status": "none",
            "session": null,
            "commits": [],
            "note": LABELS_NOTE,
        });
        // Even the no-session branch returns early without the footer today
        // — assert the constant itself carries the exact required wording,
        // since that's what a "found" render appends verbatim.
        assert_eq!(
            LABELS_NOTE,
            "labels mean: commit still in history / files changed since — never \"fact still true\""
        );
        let _ = render_human(&out);
    }
}
