//! `kb remember` / `kb recall` / `kb forget` — the agent-facing memory
//! primitives. Thin daemon wrappers: recall is the cross-corpus ranking
//! authority and remember/forget mutate corpus files, so all three need a
//! reachable daemon (no offline mode).

use crate::http;
use crate::session_marker::read_session_marker;
use anyhow::{anyhow, Result};
use std::path::Path;

/// `kb remember "<text>"` — render + POST a memory artifact.
#[allow(clippy::too_many_arguments)]
pub async fn remember(
    text: &str,
    title: Option<&str>,
    summary: Option<&str>,
    kb: Option<&str>,
    scope: Option<&str>,
    category: &str,
    tags: Option<&str>,
    salience: Option<f32>,
    decay: Option<&str>,
    supersedes: Option<&str>,
    memory_type: Option<&str>,
    source: Option<&str>,
    failed: bool,
    session_id: Option<&str>,
    no_session: bool,
    global: bool,
    link: Option<&str>,
    daemon: Option<&str>,
    bearer: Option<&str>,
    json: bool,
) -> Result<()> {
    // MI-W3.3a / MI-W3.4 — validate the closed sets BEFORE any network
    // call: a typo'd value should fail fast and locally, not round-trip to
    // the daemon just to 400.
    if let Some(t) = memory_type {
        if kb_core::memory::MemoryType::parse(t).is_none() {
            let err = anyhow!("--type must be one of: episodic, semantic, procedural; got {t:?}");
            if json {
                emit_json_error(&err, daemon);
            }
            return Err(err);
        }
    }
    if let Some(s) = source {
        if kb_core::memory::TrustSource::parse(s).is_none() {
            let err = anyhow!(
                "--source must be one of: fetched-web, user-dictated, agent-inference; got {s:?}"
            );
            if json {
                emit_json_error(&err, daemon);
            }
            return Err(err);
        }
    }
    let res = remember_inner(
        text,
        title,
        summary,
        kb,
        scope,
        category,
        tags,
        salience,
        decay,
        supersedes,
        memory_type,
        source,
        failed,
        session_id,
        no_session,
        global,
        link,
        daemon,
        bearer,
    )
    .await;
    match res {
        Ok(body) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&body)?);
            } else {
                println!(
                    "remembered {}  ({})",
                    body["id"].as_str().unwrap_or("?"),
                    body["path"].as_str().unwrap_or("?")
                );
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

/// SL3 — `pub(crate)` so `kb slate promote #n --to memory` composes the
/// EXACT same write instead of growing a second memory-ingest path: it
/// needs the minted id back (to post `done #n "promoted → mem <id>"`),
/// which the printing wrapper above does not return.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn remember_inner(
    text: &str,
    title: Option<&str>,
    summary: Option<&str>,
    kb: Option<&str>,
    scope: Option<&str>,
    category: &str,
    tags: Option<&str>,
    salience: Option<f32>,
    decay: Option<&str>,
    supersedes: Option<&str>,
    memory_type: Option<&str>,
    source: Option<&str>,
    failed: bool,
    session_id: Option<&str>,
    no_session: bool,
    global: bool,
    link: Option<&str>,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<serde_json::Value> {
    let url = require_daemon(daemon, bearer).await?;
    let (kb, auto_link) = match resolve_memory_kb(&url, kb, scope, bearer).await? {
        MemoryTarget::Corpus(name) => (name, None),
        MemoryTarget::GlobalLinked { kb, link } => (kb, link),
    };
    let title = title
        .map(str::to_string)
        .unwrap_or_else(|| derive_title(text));
    let mut tags_vec: Vec<String> = tags
        .map(|t| {
            t.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    // CT-C3 — the failed-outcome pairing, resolved CLIENT-side too (the
    // ingest route repeats it authoritatively; `ensure_failed_outcome_tag`
    // is idempotent so nothing double-tags). Appending the tag here means
    // an older daemon that ignores the unknown `outcome` field still
    // carries the indexed marker rather than silently losing it.
    let outcome = apply_failed_outcome(failed, &mut tags_vec);

    // v0.14 S1 — session-id resolution. `--session-id` wins; else read
    // the marker file the SessionStart / UserPromptSubmit hooks drop
    // at `${XDG_CACHE_HOME:-$HOME/.cache}/kb/current-session`. The
    // `--no-session` flag short-circuits both paths.
    let resolved_session = if no_session {
        None
    } else if let Some(sid) = session_id {
        Some(sid.to_string())
    } else {
        read_session_marker()
    };

    // L8 — visibility flags, now composed with MI-W0.2's auto-link and
    // MI-W3.4's untrusted-source gate. See `resolve_visibility`'s doc for
    // the full ladder (pure, unit-tested independent of the network calls
    // above that produced `auto_link`).
    let (payload_global, linked_vec) = resolve_visibility(link, global, auto_link, source);

    let mut payload = serde_json::json!({
        "title": title,
        "body": text,
        "category": category,
        "tags": tags_vec,
        "global": payload_global,
        "linked_kbs": linked_vec,
    });
    if let Some(s) = salience {
        payload["salience"] = serde_json::json!(s);
    }
    if let Some(d) = decay {
        payload["decay"] = serde_json::json!(d);
    }
    if let Some(sup) = supersedes {
        payload["supersedes"] = serde_json::json!(sup);
    }
    if let Some(sum) = summary {
        payload["summary"] = serde_json::json!(sum);
    }
    if let Some(t) = memory_type {
        payload["memory_type"] = serde_json::json!(t);
    }
    if let Some(s) = source {
        payload["source"] = serde_json::json!(s);
    }
    if let Some(o) = outcome {
        payload["outcome"] = serde_json::json!(o);
    }
    if let Some(sid) = resolved_session {
        payload["session_id"] = serde_json::json!(sid);
    }

    let client = http::client_with_timeout_and_bearer(10, bearer)?;
    let resp = client
        .post(format!(
            "{url}/api/kb/{}/artifacts",
            http::encode_path_segment(&kb)
        ))
        .json(&payload)
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("daemon returned {status}: {body}"));
    }
    Ok(resp.json().await?)
}

/// `kb recall "<query>"` — ranked fan-out across the in-scope corpora.
///
/// B1 — default scope is now `"auto"`, not `"all"`: with no project the
/// caller named, `auto` derives the current repo's `memory-<slug>` project
/// corpus (from the git MAIN checkout root, `--cwd` if given else the
/// process cwd — see `current_repo_slug_in`/`resolve_repo_root`) and narrows
/// the daemon-wide fan-out to global corpora + that one project corpus,
/// mirroring `kb remember`'s existing write-side ladder
/// (`pick_memory_target`/`resolve_memory_kb`). Outside a repo (or no git),
/// `auto` degrades to exactly today's `all` — byte-identical wire, nothing
/// narrowed. `--scope all` stays the explicit, unnarrowed everything-view —
/// pass it outright for a true cross-project fan-out (a dedup oracle like
/// `/kb-reflect` must keep doing this on purpose). `global`/`project` are
/// unchanged.
#[allow(clippy::too_many_arguments)]
pub async fn recall(
    query: &str,
    scope: &str,
    project: Option<&str>,
    cwd: Option<&str>,
    limit: usize,
    for_kb: Option<&str>,
    no_floor: bool,
    explain: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
    json: bool,
) -> Result<()> {
    let res = recall_inner(
        query, scope, project, cwd, limit, for_kb, no_floor, daemon, bearer,
    )
    .await;
    match res {
        Ok((url, body)) => {
            if json {
                let mut out = body;
                if let Some(obj) = out.as_object_mut() {
                    obj.insert("source".into(), serde_json::Value::String(url));
                }
                println!("{}", serde_json::to_string_pretty(&out)?);
            } else {
                let empty: Vec<serde_json::Value> = Vec::new();
                let hits = body["hits"].as_array().unwrap_or(&empty);
                println!(
                    "{} memories in {} ms",
                    hits.len(),
                    body["ms"].as_u64().unwrap_or(0)
                );
                for h in hits {
                    println!(
                        "  {}  {}  [{}]  score={:.4}",
                        h["id"].as_str().unwrap_or("-"),
                        h["title"].as_str().unwrap_or("-"),
                        h["kb"].as_str().unwrap_or("-"),
                        h["score"].as_f64().unwrap_or(0.0)
                    );
                    if explain {
                        println!("      = {}", explain_line(h));
                    }
                }
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

/// invariant:10 decomposition — render the `rel × salience × decay = score`
/// arithmetic from the wire fields `rerank_with_policy` surfaces
/// (`rank`/`rel`/`decay`/`age_days`), mirroring `kb resurface`'s
/// `explain_line`. Falls back to just the score when an older daemon
/// doesn't send the decomposition (additive fields, may be absent).
///
/// MI-W2.1/2.2, split MI-W5.R — when the daemon has `[memory]
/// scoring_v2_relevance`/`scoring_v2_stability` on (independently), two more
/// factors ride the SAME wire hit (`relevance_factor`/`stability`, both
/// additive + optional): appended as `× relevance=…` / `× stability=…`
/// right after `decay`, so the printed arithmetic always matches whatever
/// `rerank_with_policy_scored` actually multiplied — never re-derived.
fn explain_line(h: &serde_json::Value) -> String {
    let score = h["score"].as_f64().unwrap_or(0.0);
    let base = match (
        h["rank"].as_u64(),
        h["rel"].as_f64(),
        h["salience"].as_f64(),
        h["decay"].as_f64(),
        h["age_days"].as_f64(),
    ) {
        (Some(rank), Some(rel), Some(salience), Some(decay), Some(age_days)) => format!(
            "rel(1/(60+{rank}))={rel:.4} × salience={salience:.2} × decay(age={age_days:.1}d)={decay:.4}"
        ),
        _ => return format!("score={score:.4}"),
    };
    let mut extra = String::new();
    if let Some(rf) = h["relevance_factor"].as_f64() {
        extra.push_str(&format!(" × relevance={rf:.2}"));
    }
    if let Some(st) = h["stability"].as_f64() {
        extra.push_str(&format!(" × stability={st:.2}"));
    }
    format!("{base}{extra}  →  {score:.4}")
}

/// B1 — pure decision behind `kb recall`'s scope wiring, extracted from
/// `recall_inner` so the whole ladder is unit-testable without a daemon or
/// git. `scope`/`project` are exactly what the CLI flags carry; `slug` is
/// `current_repo_slug`/`current_repo_slug_in`'s result, pre-derived by the
/// caller (empty → `None`) so this function never shells out. Returns the
/// wire `(scope, project, visible_to)` triple.
///
/// - `scope == "auto"` + an explicit `--project`: the caller named a
///   corpus, so behave exactly like today's `--scope all --project <p>` —
///   `visible_to` stays unset (nothing to guess about a caller-named
///   target).
/// - `scope == "auto"` + no `--project` + a resolvable repo slug: narrow to
///   global corpora + this repo's own `memory-<slug>` project corpus (the
///   `project` param, mirroring `pick_memory_target`'s write-side ladder),
///   plus a `visible_to` hint (B1-additive, see `recall_inner`) so a daemon
///   that understands it can also surface memories explicitly LINKED to
///   either name (not just the corpus itself).
/// - `scope == "auto"` + no `--project` + no slug (not in a repo, or no
///   git found): degrades to exactly today's default —
///   `("all", None, None)`, byte-identical wire.
/// - any other `scope` (`"all"`/`"global"`/`"project"`): passthrough,
///   byte-identical to before B1 — never any `visible_to`.
fn resolve_recall_wire(
    scope: &str,
    project: Option<String>,
    slug: Option<String>,
) -> (String, Option<String>, Option<String>) {
    if scope != "auto" {
        return (scope.to_string(), project, None);
    }
    if let Some(p) = project {
        return ("all".to_string(), Some(p), None);
    }
    match slug {
        Some(s) => (
            "all".to_string(),
            Some(format!("memory-{s}")),
            Some(format!("{s},memory-{s}")),
        ),
        None => ("all".to_string(), None, None),
    }
}

#[allow(clippy::too_many_arguments)]
async fn recall_inner(
    query: &str,
    scope: &str,
    project: Option<&str>,
    cwd: Option<&str>,
    limit: usize,
    for_kb: Option<&str>,
    no_floor: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<(String, serde_json::Value)> {
    let url = require_daemon(daemon, bearer).await?;
    // Shell git ONLY when the caller actually asked for `auto` — every
    // other scope value must stay byte-identical to before B1, including
    // never paying for a git invocation it doesn't need.
    let slug = if scope == "auto" {
        let s = match cwd {
            Some(c) => current_repo_slug_in(&std::path::PathBuf::from(c)),
            None => current_repo_slug(),
        };
        (!s.is_empty()).then_some(s)
    } else {
        None
    };
    let (wire_scope, wire_project, wire_visible_to) =
        resolve_recall_wire(scope, project.map(str::to_string), slug);
    let mut q = format!(
        "{url}/api/memory/recall?q={}&scope={}&limit={limit}",
        http::encode_path_segment(query),
        http::encode_path_segment(&wire_scope),
    );
    if let Some(p) = &wire_project {
        q.push_str(&format!("&project={}", http::encode_path_segment(p)));
    }
    if let Some(target) = for_kb {
        q.push_str(&format!("&for_kb={}", http::encode_path_segment(target)));
    }
    if no_floor {
        q.push_str("&no_floor=true");
    }
    // B1-additive: the RUNNING prod daemon predates `visible_to`, and axum's
    // `Query` extractor silently ignores unrecognised params, so an old
    // daemon just serves `(wire_scope, wire_project)` as it always has —
    // this degrades gracefully instead of erroring. A daemon that DOES
    // understand it can use the csv to also surface memories explicitly
    // linked to the slug or its project-corpus name (see
    // `resolve_recall_wire`'s doc comment).
    if let Some(vt) = &wire_visible_to {
        q.push_str(&format!("&visible_to={}", http::encode_path_segment(vt)));
    }
    // Recall embeds the query (a large model like bge-large is ~1024-dim and
    // costly on a busy CPU) then fans out FTS + vector queries across every
    // in-scope memory corpus, so a cold/loaded recall can run ~10-20s. The
    // old 10s cap tipped over on cold starts; 30s gives comfortable headroom
    // while still bounding a genuinely-stuck daemon.
    let client = http::client_with_timeout_and_bearer(30, bearer)?;
    let resp = client.get(&q).send().await?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("daemon returned {status}: {body}"));
    }
    let body: serde_json::Value = resp.json().await?;
    Ok((url, body))
}

/// `kb forget <id>` — MI-W2.3: soft-forgets by default (tombstones the
/// artifact in place — `kb-status: forgotten`, still on disk, still listed
/// by a memory census, dropped from `recall`); `--purge` hard-deletes
/// (the pre-W2.3 behavior, irreversible). Prints which actually happened,
/// per the daemon's response (`purged`), rather than assuming.
pub async fn forget(
    id: &str,
    kb: Option<&str>,
    purge: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let url = require_daemon(daemon, bearer).await?;
    let kb = match kb {
        Some(k) => k.to_string(),
        None => http::resolve_default_kb(None, Some(&url), bearer).await?,
    };
    let client = http::client_with_timeout_and_bearer(10, bearer)?;
    let mut req = client.delete(format!(
        "{url}/api/kb/{}/artifacts/{}",
        http::encode_path_segment(&kb),
        http::encode_path_segment(id)
    ));
    if purge {
        req = req.query(&[("purge", "true")]);
    }
    let resp = req.send().await?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("daemon returned {status}: {body}"));
    }
    let body: serde_json::Value = resp.json().await.unwrap_or_default();
    let purged = body["purged"].as_bool().unwrap_or(purge);
    if purged {
        println!("purged {id} (kb {kb}) — hard-deleted, no trace left");
    } else {
        println!("forgot {id} (kb {kb}) — soft-forgotten (tombstoned, still on disk)");
    }
    Ok(())
}

/// `kb memory log <id>` — MI-W2.4a: walk one supersede chain, both
/// directions, timestamps + forgotten-state. MI-W2.4c: prints an EPOCH
/// HONESTY caveat when the walked window reaches back before the daemon's
/// tombstone era (pre-MI-W2.3 deletes were hard, unrecoverable, and
/// leave no trace this walk — or anything else — could ever surface).
pub async fn log(
    id: &str,
    kb: Option<&str>,
    daemon: Option<&str>,
    bearer: Option<&str>,
    json: bool,
) -> Result<()> {
    let url = require_daemon(daemon, bearer).await?;
    let kb = match kb {
        Some(k) => k.to_string(),
        None => http::resolve_default_kb(None, Some(&url), bearer).await?,
    };
    let client = http::client_with_timeout_and_bearer(15, bearer)?;
    let resp = client
        .get(format!(
            "{url}/api/kb/{}/memories/{}/lineage",
            http::encode_path_segment(&kb),
            http::encode_path_segment(id)
        ))
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("daemon returned {status}: {body}"));
    }
    let lineage: serde_json::Value = resp.json().await?;

    let era: serde_json::Value = client
        .get(format!("{url}/api/memory/tombstone-era"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let era_started = era["started_unix"].as_i64().unwrap_or(0);

    if json {
        let mut out = lineage;
        if let Some(obj) = out.as_object_mut() {
            obj.insert(
                "tombstone_era_started_unix".into(),
                serde_json::json!(era_started),
            );
        }
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    println!("{}", render_lineage(&lineage, era_started));
    Ok(())
}

/// Pure text renderer for `kb memory log` — split out so the EPOCH
/// HONESTY caveat logic + the timeline layout are unit-testable without a
/// daemon.
fn render_lineage(lineage: &serde_json::Value, era_started: i64) -> String {
    let mut out = String::new();
    let start = &lineage["start"];
    let supersedes = lineage["supersedes_chain"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let superseded_by = lineage["superseded_by_chain"]
        .as_array()
        .cloned()
        .unwrap_or_default();

    if superseded_by.is_empty() {
        out.push_str("(nothing supersedes this memory)\n");
    } else {
        for n in superseded_by.iter().rev() {
            out.push_str(&format!("  {}\n", format_lineage_node(n)));
            out.push_str("  │  superseded by\n");
        }
    }
    out.push_str(&format!("▶ {}\n", format_lineage_node(start)));
    if supersedes.is_empty() {
        out.push_str("(this memory supersedes nothing)\n");
    } else {
        for n in &supersedes {
            out.push_str("  │  supersedes\n");
            out.push_str(&format!("  {}\n", format_lineage_node(n)));
        }
    }

    // MI-W2.4c — the walked window's oldest known timestamp, across every
    // node this response actually carries (start + both chains). `None`
    // (no timestamped node at all) can't be compared, so no caveat fires —
    // there's nothing to say is "before" an unknown point.
    let oldest = std::iter::once(start)
        .chain(supersedes.iter())
        .chain(superseded_by.iter())
        .filter_map(|n| n["created_unix"].as_i64())
        .min();
    if let Some(oldest) = oldest {
        if oldest < era_started {
            out.push_str(&format!(
                "\nNOTE (epoch honesty): this chain reaches back to {oldest} unix, before {era_started} \
                 unix — the moment this daemon became able to soft-forget (MI-W2.3) instead of hard-\
                 deleting. Anything hard-deleted before {era_started} left no trace and cannot appear \
                 here, in `kb memory census`, or anywhere else.\n"
            ));
        }
    }
    out
}

fn format_lineage_node(n: &serde_json::Value) -> String {
    let id = n["id"].as_str().unwrap_or("?");
    let title = n["title"].as_str().unwrap_or("?");
    let created = n["created_unix"].as_i64();
    let when = created
        .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "?".to_string());
    let forgotten = n["forgotten"].as_bool().unwrap_or(false);
    let tag = if forgotten { "  [forgotten]" } else { "" };
    format!("{id}  {title}  ({when}){tag}")
}

/// `kb memory recalled-by` — CT-B2: print every session that recalled this
/// memory (the memory-side reverse of the `memory_recalls` ledger; `GET
/// /api/sessions/{sid}/recalls` is the session-side view of the same
/// table). Read-only; never mutates anything. Best-effort — "recalls the
/// capture pipeline saw", not a complete injection log.
pub async fn recalled_by(
    id: &str,
    kb: Option<&str>,
    daemon: Option<&str>,
    bearer: Option<&str>,
    json: bool,
) -> Result<()> {
    let url = require_daemon(daemon, bearer).await?;
    let kb = match kb {
        Some(k) => k.to_string(),
        None => http::resolve_default_kb(None, Some(&url), bearer).await?,
    };
    let client = http::client_with_timeout_and_bearer(15, bearer)?;
    let resp = client
        .get(format!(
            "{url}/api/kb/{}/memories/{}/recalled-by",
            http::encode_path_segment(&kb),
            http::encode_path_segment(id)
        ))
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("daemon returned {status}: {body}"));
    }
    let body: serde_json::Value = resp.json().await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    println!("{}", render_recalled_by(&body));
    Ok(())
}

/// Pure text renderer for `kb memory recalled-by` — split out so the
/// layout is unit-testable without a daemon. One line per recall: a
/// short (8-char) session id + the session's display name (falling back to
/// the short id itself when the daemon sent neither `title` nor
/// `first_user_prompt`), plus a `[turn t-…]` marker when the injection's
/// own Turn id was recovered and a `[#N]` RANK marker (MR1/V0041) when the
/// capture's `kb-recall/1` marker carried a `pos=` pair. Wording is
/// deliberately "recalls the capture pipeline saw" — a best-effort census
/// over each kb's own ledger, never a claim of completeness (a misfired
/// hook or an uncaptured session leaves a silent gap).
///
/// `pos` is absent on every pre-MR1 row and on any hit only the free-text
/// fallback grammar could parse, so the marker is OMITTED rather than
/// printed as `[#?]`: an unknown rank and a rank of nothing look the same
/// to a reader, and the honest rendering is silence.
fn render_recalled_by(body: &serde_json::Value) -> String {
    let rows = body["rows"].as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        return "no recalls the capture pipeline saw\n".to_string();
    }
    let mut out = format!(
        "{} recall{} the capture pipeline saw:\n",
        rows.len(),
        if rows.len() == 1 { "" } else { "s" }
    );
    for r in &rows {
        let sid = r["session_id"].as_str().unwrap_or("?");
        let short: String = sid.chars().take(8).collect();
        let title = r["session_title"].as_str().unwrap_or(&short);
        let when = r["recalled_at"]
            .as_i64()
            .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
            .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_else(|| "?".to_string());
        let turn = r["turn_id"]
            .as_str()
            .map(|t| format!("  [turn {t}]"))
            .unwrap_or_default();
        // MR1 — the hit's rank in the pack that injected it (1 = top).
        // Absent-when-unknown on the wire; see this fn's doc comment.
        let pos = r["pos"]
            .as_u64()
            .map(|p| format!("  [#{p}]"))
            .unwrap_or_default();
        // CT-C5 — absent-when-false wire field; "referenced" = the session
        // explicitly named this memory in a later turn (a lower bound —
        // acting on a fact without naming it reads as unreferenced).
        let used = if r["used"].as_bool().unwrap_or(false) {
            "  · referenced"
        } else {
            ""
        };
        out.push_str(&format!(
            "recalled in {title} ({short}) at {when}{pos}{turn}{used}\n"
        ));
    }
    out
}

/// `kb memory dupes` — MI-W3.1: print the on-demand cross-corpus duplicate
/// report. Read-only; never mutates anything (the operator resolves a real
/// duplicate with `kb remember --supersedes` or `kb forget`).
pub async fn dupes(
    threshold: Option<f32>,
    limit: Option<usize>,
    kb: Option<&str>,
    daemon: Option<&str>,
    bearer: Option<&str>,
    json: bool,
) -> Result<()> {
    let url = require_daemon(daemon, bearer).await?;
    let client = http::client_with_timeout_and_bearer(30, bearer)?;
    let mut req = client.get(format!("{url}/api/memory/dupes"));
    let mut q: Vec<(&str, String)> = Vec::new();
    if let Some(t) = threshold {
        q.push(("threshold", t.to_string()));
    }
    if let Some(l) = limit {
        q.push(("limit", l.to_string()));
    }
    if let Some(k) = kb {
        q.push(("kb", k.to_string()));
    }
    if !q.is_empty() {
        req = req.query(&q);
    }
    let resp = req.send().await?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("daemon returned {status}: {body}"));
    }
    let body: serde_json::Value = resp.json().await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    println!("{}", render_dupes(&body));
    Ok(())
}

/// Pure text renderer for `kb memory dupes` — split out so the table
/// layout is unit-testable without a daemon.
fn render_dupes(body: &serde_json::Value) -> String {
    let threshold = body["threshold"].as_f64().unwrap_or(0.0);
    let scanned = body["scanned"].as_u64().unwrap_or(0);
    let pairs = body["pairs"].as_array().cloned().unwrap_or_default();
    let mut out = format!(
        "{} candidate memor{} scanned, threshold {threshold:.2}\n",
        scanned,
        if scanned == 1 { "y" } else { "ies" }
    );
    if pairs.is_empty() {
        out.push_str("no likely-redundant pairs found\n");
        return out;
    }
    for p in &pairs {
        let cosine = p["cosine"].as_f64().unwrap_or(0.0);
        let cross = p["cross_corpus"].as_bool().unwrap_or(false);
        let tag = if cross { "  [cross-corpus]" } else { "" };
        out.push_str(&format!(
            "{:.3}{tag}\n  {} {} — {}\n  {} {} — {}\n",
            cosine,
            p["kb_a"].as_str().unwrap_or("?"),
            p["id_a"].as_str().unwrap_or("?"),
            p["title_a"].as_str().unwrap_or("?"),
            p["kb_b"].as_str().unwrap_or("?"),
            p["id_b"].as_str().unwrap_or("?"),
            p["title_b"].as_str().unwrap_or("?"),
        ));
    }
    out
}

/// `kb memory triage` — MI-W4.4: print the bounded, DERIVED hygiene queue.
/// Read-only; never mutates anything — act on an item with the neighboring
/// `pin`/`salience`/`remember --supersedes`/`forget` verbs.
pub async fn triage(
    kb: Option<&str>,
    limit: Option<usize>,
    daemon: Option<&str>,
    bearer: Option<&str>,
    json: bool,
) -> Result<()> {
    let url = require_daemon(daemon, bearer).await?;
    let client = http::client_with_timeout_and_bearer(30, bearer)?;
    let mut req = client.get(format!("{url}/api/memory/triage"));
    let mut q: Vec<(&str, String)> = Vec::new();
    if let Some(k) = kb {
        q.push(("kb", k.to_string()));
    }
    if let Some(l) = limit {
        q.push(("limit", l.to_string()));
    }
    if !q.is_empty() {
        req = req.query(&q);
    }
    let resp = req.send().await?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("daemon returned {status}: {body}"));
    }
    let body: serde_json::Value = resp.json().await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    println!("{}", render_triage(&body));
    Ok(())
}

/// Pure text renderer for `kb memory triage` — split out so the layout is
/// unit-testable without a daemon.
fn render_triage(body: &serde_json::Value) -> String {
    let scanned = body["scanned"].as_u64().unwrap_or(0);
    let items = body["items"].as_array().cloned().unwrap_or_default();
    let mut out = format!(
        "{} memor{} scanned, {} in the queue\n",
        scanned,
        if scanned == 1 { "y" } else { "ies" },
        items.len()
    );
    if items.is_empty() {
        out.push_str("queue is empty — nothing needs attention right now\n");
        return out;
    }
    for it in &items {
        out.push_str(&format!(
            "{:.2}  {} {} — {}\n    {}\n",
            it["urgency"].as_f64().unwrap_or(0.0),
            it["kb"].as_str().unwrap_or("?"),
            it["id"].as_str().unwrap_or("?"),
            it["title"].as_str().unwrap_or("?"),
            it["reason"].as_str().unwrap_or("?"),
        ));
    }
    out
}

/// `kb memory flag <id> --reason "…" [--kb NAME]` — CT-C1: the missing
/// in-session correction verb. An agent that discovers a recalled memory is
/// WRONG has had nothing between silence and a full `kb forget`/`remember
/// --supersedes`; this rides kb-comments/1 (invariant #6) rather than
/// inventing new storage — it POSTs an ordinary `author: claude` comment,
/// body `"[kb-flag] <reason>"`, through the EXISTING `add` route
/// (`POST …/review/{id}/comments`). Refuses (clearly, no write) when an
/// OPEN `[kb-flag]` comment already exists on this memory — resolve that
/// one first (`kb comments resolve` / a reply) rather than piling up
/// duplicate flags for the same complaint.
pub async fn flag(
    id: &str,
    kb: Option<&str>,
    reason: &str,
    daemon: Option<&str>,
    bearer: Option<&str>,
    json: bool,
) -> Result<()> {
    let reason = reason.trim();
    if reason.is_empty() {
        anyhow::bail!("--reason must not be empty");
    }
    let url = require_daemon(daemon, bearer).await?;
    let kb = resolve_memory_kb_for_id(&url, kb, id, bearer).await?;

    if let Some(existing) = find_open_flag_comment(&url, &kb, id, bearer).await? {
        anyhow::bail!(
            "{kb}/{id} is already flagged (open comment {existing}) — resolve it \
             first (`kb comments resolve {existing} --kb {kb} --artifact-id {id}` \
             or a reply) before flagging again"
        );
    }

    let body = format!("{}{reason}", kb_core::memory::FLAG_COMMENT_PREFIX);
    let client = http::client_with_timeout_and_bearer(10, bearer)?;
    let resp = client
        .post(format!(
            "{url}/api/kb/{}/review/{}/comments",
            http::encode_path_segment(&kb),
            http::encode_path_segment(id),
        ))
        .json(&serde_json::json!({
            "body": body,
            "anchor": {"kind": "file"},
            "author": "claude",
        }))
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow!("flag {kb}/{id} failed: HTTP {status} — {text}"));
    }
    let created: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
    if json {
        println!("{}", serde_json::to_string_pretty(&created)?);
        return Ok(());
    }
    println!(
        "✓ flagged {kb}/{id} ({}) — {reason}",
        created["id"].as_str().unwrap_or("?"),
    );
    Ok(())
}

/// Resolve `<id>` to its owning kb for `flag`: `--kb` wins verbatim;
/// absent, search every memory-scoped kb on the daemon (`GET /api/kbs`,
/// the SAME footprint `kb recall`/`kb memory triage` cover) for the first
/// one holding this id (`GET /api/kb/{kb}/docs/{id}`).
async fn resolve_memory_kb_for_id(
    url: &str,
    kb: Option<&str>,
    id: &str,
    bearer: Option<&str>,
) -> Result<String> {
    if let Some(k) = kb {
        return Ok(k.to_string());
    }
    let client = http::client_with_timeout_and_bearer(5, bearer)?;
    let kbs: serde_json::Value = client
        .get(format!("{url}/api/kbs"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let candidates: Vec<String> = kbs
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or(&[])
        .iter()
        .filter(|k| matches!(k["memory_scope"].as_str(), Some("project") | Some("global")))
        .filter_map(|k| k["name"].as_str().map(str::to_string))
        .collect();
    if candidates.is_empty() {
        anyhow::bail!("no memory-scoped kbs configured; pass --kb <name>");
    }
    for k in &candidates {
        let doc_url = format!(
            "{url}/api/kb/{}/docs/{}",
            http::encode_path_segment(k),
            http::encode_path_segment(id)
        );
        let resp = client.get(&doc_url).send().await?;
        match resp.status().as_u16() {
            200 => return Ok(k.clone()),
            404 => continue,
            s => {
                let body = resp.text().await.unwrap_or_default();
                anyhow::bail!("GET {doc_url}: HTTP {s}: {body}");
            }
        }
    }
    anyhow::bail!(
        "memory {id} not found in any memory-scoped kb (searched: {}); pass --kb <name>",
        candidates.join(", ")
    )
}

/// Dedup guard for `flag`: is there already an OPEN `[kb-flag]` comment on
/// this artifact? `GET …/reviews?status=open&artifact_id=` is the exact
/// read `kb comments list --path` uses; this just filters its rows for the
/// flag tag. Returns the existing comment's id when found.
async fn find_open_flag_comment(
    url: &str,
    kb: &str,
    id: &str,
    bearer: Option<&str>,
) -> Result<Option<String>> {
    let client = http::client_with_timeout_and_bearer(10, bearer)?;
    let body: serde_json::Value = client
        .get(format!(
            "{url}/api/kb/{}/reviews",
            http::encode_path_segment(kb)
        ))
        .query(&[("status", "open"), ("artifact_id", id)])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let rows = body["comments"].as_array().cloned().unwrap_or_default();
    Ok(rows
        .into_iter()
        .find(|r| {
            r["body"]
                .as_str()
                .map(kb_core::memory::is_flag_comment)
                .unwrap_or(false)
        })
        .and_then(|r| r["comment_id"].as_str().map(str::to_string)))
}

/// `kb memory expand <id>` — CT-B4: from a highlight-born memory's
/// one-liner back to the origin passage it was lifted from. `--kb` names
/// the memory's OWN corpus (not the origin's — the origin kb/artifact are
/// read off the memory itself); omitted, every memory-scoped kb is
/// searched (mirrors `kb memory log`/`triage`'s corpus resolution, widened
/// into a search since `expand` is often reached straight from a bare
/// recall-hit id with no corpus in hand).
///
/// U3 writes a highlight-born memory's origin (`kb-source-kb`/
/// `kb-source-artifact`/`kb-source-anchor`) onto the MEMORY ARTIFACT'S OWN
/// `<meta>` tags at write time (`kb_core::memory::render_artifact`) — but in
/// this tree nothing parses them back off anywhere server-side yet (no
/// census/recall field carries them; `MemoryProvenance`'s own doc comment
/// calls this out: "write-only"). So this verb reads them the same way `kb
/// cat` reads any artifact's raw HTML — fetching the memory's OWN bytes via
/// the shared `cat::fetch_artifact_bytes` and picking the three metas back
/// out — rather than depending on a read-side route that doesn't exist yet.
///
/// Once the origin is known, this reuses kb-comments/1's EXISTING anchor
/// resolution ladder verbatim (`kb_core::review::fuzzy_resolve_anchor` +
/// its `kb_core::lists::section_passage` sibling) — never a second
/// heuristic — to confirm the anchor still resolves in the origin's CURRENT
/// source and print the surrounding passage. A memory with no recorded
/// origin, an origin artifact that's gone entirely, or an anchor that no
/// longer resolves each render an explicit, honest line — never a guessed
/// passage.
pub async fn expand(
    id: &str,
    kb: Option<&str>,
    daemon: Option<&str>,
    bearer: Option<&str>,
    json: bool,
) -> Result<()> {
    let res = expand_inner(id, kb, daemon, bearer).await;
    match res {
        Ok(out) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&out)?);
            } else {
                println!("{}", render_expand(&out));
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

async fn expand_inner(
    id: &str,
    kb: Option<&str>,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<serde_json::Value> {
    let url = require_daemon(daemon, bearer).await?;
    let (kb_name, title) = resolve_memory_doc(&url, kb, id, bearer).await?;
    let memory = serde_json::json!({ "id": id, "kb": kb_name, "title": title });

    let bytes = super::cat::fetch_artifact_bytes(&url, &kb_name, id, bearer).await?;
    let html = String::from_utf8_lossy(&bytes).into_owned();
    let source_kb = meta_content(&html, "kb-source-kb");
    let source_artifact = meta_content(&html, "kb-source-artifact");

    let (Some(source_kb), Some(source_artifact)) = (source_kb, source_artifact) else {
        return Ok(serde_json::json!({
            "memory": memory,
            "source": null,
            "anchor": null,
            "resolved": false,
            "passage": null,
        }));
    };

    // Best-effort parse; a missing or malformed anchor meta degrades to
    // `File` (whole-artifact — never a wrong guessed spot) rather than
    // failing the whole verb, but the JSON `anchor` field below still
    // reports what was ACTUALLY stored (`stored_anchor`, `None` when
    // absent/unparseable) — never claiming a `File` anchor was recorded
    // when it wasn't.
    let stored_anchor: Option<kb_core::review::Anchor> = meta_content(&html, "kb-source-anchor")
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok());
    let effective_anchor = stored_anchor
        .clone()
        .unwrap_or(kb_core::review::Anchor::File);

    let Some(origin_title) = doc_title(&url, &source_kb, &source_artifact, bearer).await? else {
        return Ok(serde_json::json!({
            "memory": memory,
            "source": {"kb": source_kb, "artifact": source_artifact, "title": null},
            "anchor": stored_anchor,
            "resolved": false,
            "passage": null,
        }));
    };

    let origin_bytes =
        super::cat::fetch_artifact_bytes(&url, &source_kb, &source_artifact, bearer).await?;
    let origin_html = String::from_utf8_lossy(&origin_bytes).into_owned();
    let resolution = kb_core::review::fuzzy_resolve_anchor(&origin_html, &effective_anchor);
    let resolved = !matches!(resolution, kb_core::review::Resolution::Stale);
    let passage = if resolved {
        passage_for(&origin_html, &effective_anchor, &resolution)
    } else {
        None
    };

    Ok(serde_json::json!({
        "memory": memory,
        "source": {"kb": source_kb, "artifact": source_artifact, "title": origin_title},
        "anchor": stored_anchor,
        "resolved": resolved,
        "passage": passage,
    }))
}

/// The printed passage for a resolved (non-stale) anchor. `Selection`
/// resolves via `fuzzy_resolve_anchor`'s own matched block text — the
/// CURRENT paragraph/list-item/etc it found — with no separate DOM walk
/// (re-deriving the same match a second way would be a second resolution
/// heuristic); `Section`/`Chapter` delegate to `kb_core::lists::
/// section_passage` (the review ladder's sibling for "the surrounding
/// section body"). `File` has no single passage — a whole-artifact
/// highlight — so this returns `None` and the renderer says so explicitly.
fn passage_for(
    html: &str,
    anchor: &kb_core::review::Anchor,
    resolution: &kb_core::review::Resolution,
) -> Option<String> {
    match anchor {
        kb_core::review::Anchor::Selection { .. } => match resolution {
            kb_core::review::Resolution::Exact(s) | kb_core::review::Resolution::Fuzzy(s, _) => {
                Some(cap_selection_passage(s))
            }
            kb_core::review::Resolution::Stale => None,
        },
        kb_core::review::Anchor::File => None,
        kb_core::review::Anchor::Section { .. } | kb_core::review::Anchor::Chapter { .. } => {
            kb_core::lists::section_passage(html, anchor, resolution)
        }
    }
}

/// Bound a Selection match's own text to the SAME budget `section_passage`
/// bounds a Section/Chapter passage to (`kb_core::lists::
/// SECTION_PASSAGE_CHAR_CAP`) — one shared cap regardless of which anchor
/// scope produced the text.
fn cap_selection_passage(s: &str) -> String {
    let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= kb_core::lists::SECTION_PASSAGE_CHAR_CAP {
        return collapsed;
    }
    let capped: String = collapsed
        .chars()
        .take(kb_core::lists::SECTION_PASSAGE_CHAR_CAP)
        .collect();
    let truncated = match capped.rfind(char::is_whitespace) {
        Some(idx) => capped[..idx].trim_end(),
        None => capped.as_str(),
    };
    format!("{truncated}…")
}

/// Resolve `<id>` to `(kb_name, title)`. `--kb` wins outright (one GET);
/// absent, search every memory-scoped kb (`GET /api/kbs`, the same
/// `memory_scope` footprint `kb recall`/`kb memory census` cover) for the
/// first one holding this id.
async fn resolve_memory_doc(
    url: &str,
    kb: Option<&str>,
    id: &str,
    bearer: Option<&str>,
) -> Result<(String, String)> {
    let candidates: Vec<String> = match kb {
        Some(k) => vec![k.to_string()],
        None => memory_scoped_kbs(url, bearer).await?,
    };
    if candidates.is_empty() {
        return Err(anyhow!("no memory-scoped kbs configured; pass --kb <name>"));
    }
    for k in &candidates {
        if let Some(title) = doc_title(url, k, id, bearer).await? {
            return Ok((k.clone(), title));
        }
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
/// look at, reused here so `--kb`-less `expand` searches the same
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

/// `GET /api/kb/{kb}/docs/{id}`'s title, or `None` on a 404 (this kb
/// doesn't hold the id — the caller tries the next candidate, or — when
/// `kb`/`id` are already known precisely, as for the ORIGIN artifact —
/// treats it as "gone entirely"). Any other non-success status is a real
/// error, not a "try elsewhere"/"gone" signal.
async fn doc_title(url: &str, kb: &str, id: &str, bearer: Option<&str>) -> Result<Option<String>> {
    let client = http::client_with_timeout_and_bearer(10, bearer)?;
    let doc_url = format!(
        "{url}/api/kb/{}/docs/{}",
        http::encode_path_segment(kb),
        http::encode_path_segment(id)
    );
    let resp = client.get(&doc_url).send().await?;
    if resp.status().as_u16() == 404 {
        return Ok(None);
    }
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("GET {doc_url}: HTTP {status}: {body}"));
    }
    let body: serde_json::Value = resp.json().await?;
    Ok(Some(body["title"].as_str().unwrap_or("?").to_string()))
}

/// Extract `<meta name="NAME" content="...">`'s (HTML-unescaped) `content`
/// value from a memory artifact's raw HTML. Dependency-free (no HTML parser
/// pulled into kb-cli): `kb_core::memory::render_artifact` — the ONE writer
/// of these three metas — always emits `name="..."` before `content="..."`,
/// double-quoted (its own `escape_attr`), on one line, so this scans for
/// that literal shape rather than adding a full HTML parser for three
/// known, self-authored meta names.
fn meta_content(html: &str, name: &str) -> Option<String> {
    let needle = format!(r#"<meta name="{name}" content=""#);
    let start = html.find(&needle)? + needle.len();
    let end = start + html[start..].find('"')?;
    Some(unescape_attr(&html[start..end]))
}

/// Inverse of `kb_core::memory`'s private `escape_attr` (`&` escaped to
/// `&amp;` FIRST, then `<`/`>`/`"`) — undone in the opposite order so a
/// stored `&amp;` is never double-unescaped into something the source
/// never contained.
fn unescape_attr(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&gt;", ">")
        .replace("&lt;", "<")
        .replace("&amp;", "&")
}

// ---- rendering (kb memory expand) ---------------------------------------

/// Pure text renderer for `kb memory expand` — split out so every
/// honest-degradation branch (no origin / origin gone / stale anchor /
/// resolved) is unit-testable without a daemon.
fn render_expand(out: &serde_json::Value) -> String {
    let mem = &out["memory"];
    let title = mem["title"].as_str().unwrap_or("?");
    let id = mem["id"].as_str().unwrap_or("?");
    let kb = mem["kb"].as_str().unwrap_or("?");

    let mut s = format!("{title}  ({kb}/{id})\n\n");

    if out["source"].is_null() {
        s.push_str("not a highlight-born memory — no origin recorded\n");
        return s;
    }

    let source = &out["source"];
    let source_kb = source["kb"].as_str().unwrap_or("?");
    let source_artifact = source["artifact"].as_str().unwrap_or("?");

    let Some(source_title) = source["title"].as_str() else {
        s.push_str(&format!(
            "origin artifact no longer exists — kb {source_kb}, id {source_artifact}\n"
        ));
        return s;
    };

    if !out["resolved"].as_bool().unwrap_or(false) {
        s.push_str("anchor no longer resolves — the source has changed since the highlight\n");
        s.push_str(&format!(
            "origin: {source_title}  ({source_kb}/{source_artifact})\n"
        ));
        s.push_str(&format!(
            "open it: kb cat {source_artifact} --kb {source_kb}\n"
        ));
        return s;
    }

    s.push_str(&format!(
        "from: {source_title}{}\n\n",
        anchor_header(&out["anchor"])
    ));
    match out["passage"].as_str() {
        Some(p) => s.push_str(p),
        None => s.push_str("(no specific passage — this highlight anchors the whole artifact)"),
    }
    s.push('\n');
    s
}

/// `"  — section \"id\""` / `"  — Chapter > Path"` / `"  — selection"` /
/// `"  — whole artifact"` — names the resolved anchor scope, appended after
/// the origin artifact's title. `Value::Null` (no anchor recorded, or an
/// unrecognised `kind`) yields an empty suffix rather than guessing one.
fn anchor_header(anchor: &serde_json::Value) -> String {
    match anchor["kind"].as_str() {
        Some("section") => anchor["id"]
            .as_str()
            .map(|id| format!("  — section {id:?}"))
            .unwrap_or_default(),
        Some("chapter") => anchor["path"]
            .as_str()
            .map(|p| format!("  — {p}"))
            .unwrap_or_default(),
        Some("selection") => "  — selection".to_string(),
        Some("file") => "  — whole artifact".to_string(),
        _ => String::new(),
    }
}

// ---- helpers ---------------------------------------------------------

async fn require_daemon(daemon: Option<&str>, bearer: Option<&str>) -> Result<String> {
    http::detect_daemon(daemon, bearer).await.ok_or_else(|| {
        anyhow!(
            "daemon not reachable{} — start it with `kb daemon`",
            daemon.map(|d| format!(" at {d}")).unwrap_or_default()
        )
    })
}

/// MI-W0.2 — where a bare `kb remember` (no `--kb`) resolves to.
/// `Corpus` is a plain target name (an explicit `--kb`, a project's own
/// dedicated memory corpus, or the fleet's single `memory_scope="global"`
/// corpus when `--scope global` was requested explicitly). `GlobalLinked`
/// is the fallback that carries `resolve_memory_kb`'s auto-link decision
/// forward to `remember_inner`: `kb` is still the global corpus to POST to,
/// `link` is `Some(slug)` when a kb named exactly the caller's repo slug
/// exists on the fleet (so the memory should be scoped to it rather than
/// truly global) or `None` when there's nothing to scope to.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum MemoryTarget {
    Corpus(String),
    GlobalLinked { kb: String, link: Option<String> },
}

/// Resolve the target memory corpus for a bare `kb remember`: `--kb` wins
/// verbatim; otherwise fetch the fleet-wide `/api/kbs` list, derive the
/// caller's repo slug, and run `pick_memory_target`'s ladder.
async fn resolve_memory_kb(
    daemon: &str,
    kb: Option<&str>,
    scope: Option<&str>,
    bearer: Option<&str>,
) -> Result<MemoryTarget> {
    if let Some(k) = kb {
        return Ok(MemoryTarget::Corpus(k.to_string()));
    }
    let want = scope.unwrap_or("project");
    let client = http::client_with_timeout_and_bearer(5, bearer)?;
    let kbs: serde_json::Value = client
        .get(format!("{daemon}/api/kbs"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let slug = current_repo_slug();
    pick_memory_target(&kbs, want, &slug)
}

/// CT-C3 — pure wiring behind `kb remember --failed`, extracted from
/// `remember_inner` so it's unit-testable without a daemon. When `failed`:
/// appends the `outcome:failed` tag (idempotent, via the ONE kb-core
/// grammar — `ensure_failed_outcome_tag`) and returns the wire `outcome`
/// value the payload sends. When not: touches nothing, sends nothing —
/// the ordinary `kb remember` payload stays byte-identical.
fn apply_failed_outcome(failed: bool, tags: &mut Vec<String>) -> Option<&'static str> {
    if !failed {
        return None;
    }
    kb_core::memory::ensure_failed_outcome_tag(tags);
    Some(kb_core::memory::MemoryOutcome::Failed.as_str())
}

/// MI-W3.4 — pure decision function behind the `(global, linked_kbs)`
/// payload fields, extracted from `remember_inner` so the whole ladder
/// (including the untrusted-source gate) is unit-testable without a
/// daemon. `source` is the raw `--source` string (validated by the caller
/// already; re-parsed here defensively — an unrecognised value is simply
/// not `fetched-web`, never a panic).
///
/// - `link` (an explicit `--link a,b`) wins outright: scoped, not global.
/// - else explicit `--global` wins: global, unconditionally — this is the
///   ONE thing that overrides MI-W3.4's gate below.
/// - else an `auto_link` (a project kb `resolve_memory_kb` already found by
///   repo slug) wins: scoped to it, not global — the gate below is moot
///   here since the write was never going to be global anyway.
/// - else, MI-W3.4: `--source fetched-web` with none of the above flips
///   the remaining "nothing else applies" default from global to
///   non-global (printing a one-line stderr notice so the flip is never
///   silent) — this is the untrusted-origin blast-radius narrowing.
/// - else (the pre-MI-W3.4 behaviour, byte-for-byte): global by default.
///
/// MI-W3.R — scope note (deliberate, not an oversight): this gate lives
/// ONLY here, narrowing the CLI's *default* when the caller didn't say
/// what they wanted. `POST /api/kb/{kb}/artifacts` (`routes::artifacts::
/// ingest`, kb-server) has no equivalent — it reads `body.global`/
/// `body.linked_kbs` verbatim, because a direct API caller (curl, a
/// non-`kb`-CLI agent, `routes::proposals::approve`'s internal call)
/// supplies visibility explicitly (or accepts ingest's own always-global
/// default) and is therefore unaffected by a CLI-only default flip — there
/// is no "the user didn't say" case to narrow on that path. See the mirror
/// comment at `routes::artifacts::ingest`'s `let global = …` line.
fn resolve_visibility(
    link: Option<&str>,
    global: bool,
    auto_link: Option<String>,
    source: Option<&str>,
) -> (bool, Vec<String>) {
    let untrusted_web_source = source
        .and_then(kb_core::memory::TrustSource::parse)
        .is_some_and(|t| t == kb_core::memory::TrustSource::FetchedWeb);
    if let Some(l) = link {
        (
            false,
            l.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
        )
    } else if global {
        (true, Vec::new())
    } else if let Some(auto) = auto_link {
        (false, vec![auto])
    } else if untrusted_web_source {
        eprintln!(
            "note: --source fetched-web with no --global (and no resolvable project kb) — \
             defaulting to a non-global memory instead of daemon-wide global, to narrow the \
             blast radius of untrusted content; pass --global explicitly to force the old \
             (always-global) default"
        );
        (false, Vec::new())
    } else {
        (true, Vec::new())
    }
}

/// Pure decision function behind `resolve_memory_kb` — no I/O, so the whole
/// ladder is unit-testable against fixture `/api/kbs` JSON.
///
/// The old rule was "the single fleet-wide corpus tagged
/// `memory_scope="project"`", with zero awareness of which project the
/// caller was actually in. On a shared daemon that has exactly one such
/// corpus (kb's own), EVERY project's bare `kb remember` landed there —
/// 77 of 245 files in kb's memory corpus belonged to other projects
/// (operator ruling 2026-08-05). The ladder below is topology-aware
/// instead: global + opt-in per-project.
///
/// - `want == "global"`: the single corpus with `memory_scope="global"`.
///   0 matches or >1 match is an error (pass `--kb`).
/// - `want == "project"` (the default): (a) a corpus named exactly
///   `memory-<slug>` with `memory_scope="project"` wins outright; (b)
///   otherwise fall back to the single `memory_scope="global"` corpus,
///   auto-linking to a kb named exactly `<slug>` when one exists (any
///   corpus, not just memory ones) so the memory is scoped to the
///   caller's own project instead of truly global. No global corpus
///   either is an error naming both misses.
/// - any other `want`: unchanged from today — filtered against
///   `memory_scope`, so an unrecognized scope surfaces as the 0-match
///   error.
fn pick_memory_target(kbs: &serde_json::Value, want: &str, slug: &str) -> Result<MemoryTarget> {
    let arr = kbs
        .as_array()
        .ok_or_else(|| anyhow!("GET /api/kbs: expected an array"))?;
    if want == "project" {
        let project_name = format!("memory-{slug}");
        let has_project_corpus = arr.iter().any(|k| {
            k["name"].as_str() == Some(project_name.as_str())
                && k["memory_scope"].as_str() == Some("project")
        });
        if has_project_corpus {
            return Ok(MemoryTarget::Corpus(project_name));
        }
        let global_matches: Vec<&str> = scope_matches(arr, "global");
        if global_matches.is_empty() {
            return Err(anyhow!(
                "no project memory corpus {project_name:?} and no global memory corpus \
                 (memory_scope=\"global\") either; pass --kb <name>"
            ));
        }
        let kb = pick_global_write_target(&global_matches)?.to_string();
        let link = arr
            .iter()
            .any(|k| k["name"].as_str() == Some(slug))
            .then(|| slug.to_string());
        return Ok(MemoryTarget::GlobalLinked { kb, link });
    }
    if want == "global" {
        let candidates = scope_matches(arr, "global");
        return pick_global_write_target(&candidates)
            .map(|name| MemoryTarget::Corpus(name.to_string()));
    }
    let matches = scope_matches(arr, want);
    match matches.len() {
        0 => Err(anyhow!(
            "no memory corpus with memory_scope={want:?}; pass --kb <name>"
        )),
        1 => Ok(MemoryTarget::Corpus(matches[0].to_string())),
        _ => Err(anyhow!(
            "multiple {want} memory corpora ({matches:?}); pass --kb <name>"
        )),
    }
}

/// Live-regression fix (hotfix following MI-W0.2, commit 7859205e) — pick a
/// WRITE target among corpora already filtered to `memory_scope="global"`.
///
/// A session-transcript corpus is *also* tagged `memory_scope="global"` (so
/// its rows join the recall fan-out, where invariant #11's R0 rule then
/// excludes `kb-category=memory-session` from results) — but it must never
/// be treated as a curated-memory WRITE target. On a real fleet this means
/// the naive "the single global-scoped corpus" rule (MI-W0.2's original
/// assumption) breaks the moment a `sessions` corpus exists alongside
/// `memory`: both fallback (`want == "project"`) and explicit
/// (`want == "global"`) write paths saw 2 matches and hard-errored,
/// dropping every bare `kb remember` outside kb's own repo.
///
/// `GET /api/kbs`'s `KbSummary` exposes no explicit "writable memory
/// corpus" marker today (`name`/`path`/`doc_count`/`last_index_at`/
/// `memory_scope`/`default_search_category` — the last unset in practice),
/// so until one exists, the conventional name `"memory"` is the
/// disambiguator: prefer it when present among the candidates; with no
/// `"memory"` and exactly one candidate there's nothing ambiguous to
/// resolve; otherwise (2+ candidates, none named `"memory"`) keep today's
/// multi-match error naming every candidate.
///
/// TODO: replace the name convention with an explicit per-kb config marker
/// once one exists (needs a prod daemon restart to pick up config — out of
/// scope for this hotfix).
fn pick_global_write_target<'a>(candidates: &[&'a str]) -> Result<&'a str> {
    if candidates.contains(&"memory") {
        return Ok("memory");
    }
    match candidates {
        [] => Err(anyhow!(
            "no memory corpus with memory_scope=\"global\"; pass --kb <name>"
        )),
        [only] => Ok(only),
        _ => Err(anyhow!(
            "multiple global memory corpora ({candidates:?}); pass --kb <name>"
        )),
    }
}

/// Corpus names on `arr` whose `memory_scope` equals `want`.
fn scope_matches<'a>(arr: &'a [serde_json::Value], want: &str) -> Vec<&'a str> {
    arr.iter()
        .filter(|k| k["memory_scope"].as_str() == Some(want))
        .filter_map(|k| k["name"].as_str())
        .collect()
}

/// Caller-side repo identity feeding the auto-link ladder AND `kb recall
/// --scope auto` (B1): the MAIN checkout root's basename, slugified via
/// `repo_slug`. Best-effort: an empty or unresolvable directory just yields
/// an empty slug, which matches no corpus and falls through to the plain
/// global target. See `resolve_repo_root` for why "main checkout root" (not
/// "whichever worktree you're standing in") is load-bearing here.
pub(crate) fn current_repo_slug() -> String {
    repo_slug(&resolve_repo_root(None))
}

/// `current_repo_slug`, but rooted at an explicit directory (e.g. `kb
/// recall --cwd`/`kb context --cwd`) instead of the process cwd. Reuses the
/// exact same git logic via `resolve_repo_root(Some(dir))` — no duplicated
/// git-shelling.
pub(crate) fn current_repo_slug_in(dir: &Path) -> String {
    repo_slug(&resolve_repo_root(Some(dir)))
}

/// Shared git-root resolution behind `current_repo_slug`/
/// `current_repo_slug_in`. Prefers the MAIN checkout root over whichever
/// worktree happens to be checked out at `dir`:
///
/// `git rev-parse --show-toplevel` (the pre-B1 behaviour) returns the
/// WORKTREE root — inside a linked worktree (e.g.
/// `.claude/worktrees/foo`) that's the worktree's own directory, not the
/// repo's main checkout, so the derived slug (`foo`) fragments memory away
/// from the project's real corpus. `git rev-parse --git-common-dir` instead
/// names the ONE `.git` directory every worktree of a repo shares
/// (conventionally `<main-root>/.git`), so walking up one level from it
/// recovers the main root regardless of which worktree you're standing in.
///
/// Ladder: common-dir → `main_root_from_common_dir` (pure, unit-tested)
/// first; a bare repo / relocated `--git-dir` / anything else
/// `main_root_from_common_dir` refuses to guess about falls back to
/// `--show-toplevel` (today's pre-fix behaviour, unchanged for every
/// non-worktree checkout); no git at all (or neither call resolves) falls
/// back to `dir` itself, then the process cwd, then empty.
///
/// `dir: None` runs every git call with no `-C` (byte-identical invocation
/// to before this fix, when the caller means "the process's own cwd");
/// `Some(d)` runs each with `-C d` instead — one code path for both.
fn resolve_repo_root(dir: Option<&Path>) -> std::path::PathBuf {
    let run = |args: &[&str]| -> Option<String> {
        let mut cmd = std::process::Command::new("git");
        if let Some(d) = dir {
            cmd.arg("-C").arg(d);
        }
        cmd.args(args)
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
    };
    run(&["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .and_then(|s| main_root_from_common_dir(&s))
        .or_else(|| {
            run(&["rev-parse", "--show-toplevel"]).map(|s| std::path::PathBuf::from(s.trim()))
        })
        .or_else(|| dir.map(Path::to_path_buf))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default()
}

/// Pure step behind `resolve_repo_root`: `git --git-common-dir`'s stdout →
/// the main checkout root, or `None` when this isn't the conventional
/// `<root>/.git` layout (a bare repo, a relocated `--git-dir`, or the bare
/// string `.git` with no parent to walk up to) — refusing to guess rather
/// than returning something wrong. Trims surrounding whitespace (git
/// appends a trailing newline) before matching the `/.git` suffix.
fn main_root_from_common_dir(common_dir: &str) -> Option<std::path::PathBuf> {
    let trimmed = common_dir.trim();
    let parent = trimmed.strip_suffix("/.git")?;
    if parent.is_empty() {
        return None;
    }
    Some(std::path::PathBuf::from(parent))
}

/// Slugify a directory's basename: lowercase, collapse every run of
/// non-alphanumeric characters to a single `-`, trim leading/trailing `-`.
/// Mirrors the hooks' shell `kb_slugify` (`kb-wake.sh`), applied to a
/// basename rather than a full path so the memory-corpus naming convention
/// (`memory-<slug>`) reads as a short project name, not an escaped path.
pub(crate) fn repo_slug(dir: &Path) -> String {
    let basename = dir
        .file_name()
        .map(|s| s.to_string_lossy())
        .unwrap_or_default();
    let mut out = String::with_capacity(basename.len());
    let mut prev_dash = false;
    for c in basename.chars() {
        let lc = c.to_ascii_lowercase();
        if lc.is_ascii_alphanumeric() {
            out.push(lc);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

/// First non-empty line, trimmed + capped, as the memory's title. The cap
/// is word-boundary-aware: a first line over `DERIVE_TITLE_CAP` chars is cut
/// at the last whitespace at or before the cap (never mid-word) and gets a
/// trailing "…"; a single word longer than the cap has no boundary to cut
/// at, so it hard-cuts at the cap instead.
const DERIVE_TITLE_CAP: usize = 160;

fn derive_title(text: &str) -> String {
    let first = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("memory");
    if first.chars().count() <= DERIVE_TITLE_CAP {
        return first.to_string();
    }
    let capped: String = first.chars().take(DERIVE_TITLE_CAP).collect();
    let truncated = match capped.rfind(char::is_whitespace) {
        Some(idx) => capped[..idx].trim_end(),
        None => capped.as_str(),
    };
    if truncated.is_empty() {
        "memory".to_string()
    } else {
        format!("{truncated}…")
    }
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

    #[test]
    fn derive_title_uses_first_nonempty_line() {
        assert_eq!(derive_title("\n  hello world  \nmore"), "hello world");
        assert_eq!(derive_title("   \n  "), "memory");
    }

    #[test]
    fn derive_title_short_title_is_unchanged() {
        let title = derive_title("a short title");
        assert_eq!(title, "a short title");
        assert!(!title.ends_with('…'));
    }

    #[test]
    fn derive_title_hard_cuts_a_200_char_single_word_at_the_cap() {
        // No whitespace anywhere in the line, so there's no word boundary to
        // cut at — falls back to a hard cut at DERIVE_TITLE_CAP.
        let long = "x".repeat(200);
        let title = derive_title(&long);
        assert_eq!(title, format!("{}…", "x".repeat(DERIVE_TITLE_CAP)));
    }

    #[test]
    fn derive_title_cuts_a_long_sentence_at_a_word_boundary() {
        let sentence = "word ".repeat(40); // 200 chars, well past the cap
        let sentence = sentence.trim();
        let title = derive_title(sentence);
        assert!(title.ends_with('…'), "expected an ellipsis, got {title:?}");
        let body = title.trim_end_matches('…');
        assert!(
            sentence.starts_with(body),
            "truncated body {body:?} must be a verbatim prefix of the source line"
        );
        assert!(
            !body.ends_with(' '),
            "must not leave a trailing space before the ellipsis"
        );
        assert!(
            body.chars().count() <= DERIVE_TITLE_CAP,
            "must not exceed the cap"
        );
    }

    // invariant:10 rank-salience-decay
    #[test]
    fn explain_line_renders_the_decomposition() {
        let h = serde_json::json!({
            "rank": 2, "rel": 0.032_258_064_5, "salience": 0.5,
            "decay": 0.904_837_4, "age_days": 10.0, "score": 0.014_598_7,
        });
        assert_eq!(
            explain_line(&h),
            "rel(1/(60+2))=0.0323 × salience=0.50 × decay(age=10.0d)=0.9048  →  0.0146"
        );
    }

    #[test]
    fn explain_line_falls_back_to_score_when_decomposition_absent() {
        let h = serde_json::json!({ "score": 0.25 });
        assert_eq!(explain_line(&h), "score=0.2500");
    }

    // ---- MI-W2.4a/c — kb memory log rendering --------------------------

    fn node(
        id: &str,
        title: &str,
        created_unix: Option<i64>,
        forgotten: bool,
    ) -> serde_json::Value {
        let mut v = serde_json::json!({ "id": id, "title": title, "forgotten": forgotten });
        if let Some(c) = created_unix {
            v["created_unix"] = serde_json::json!(c);
        }
        v
    }

    #[test]
    fn format_lineage_node_renders_id_title_date_and_forgotten_tag() {
        let n = node("abc123", "A Fact", Some(1_700_000_000), false);
        assert_eq!(format_lineage_node(&n), "abc123  A Fact  (2023-11-14)");

        let gone = node("def456", "Old Fact", Some(1_700_000_000), true);
        assert_eq!(
            format_lineage_node(&gone),
            "def456  Old Fact  (2023-11-14)  [forgotten]"
        );

        let no_date = node("ghi789", "No Date", None, false);
        assert_eq!(format_lineage_node(&no_date), "ghi789  No Date  (?)");
    }

    #[test]
    fn render_lineage_no_chain_says_so_both_directions() {
        let lineage = serde_json::json!({
            "id": "abc123",
            "start": node("abc123", "Solo Fact", Some(1_700_000_000), false),
            "supersedes_chain": [],
            "superseded_by_chain": [],
        });
        let out = render_lineage(&lineage, 0);
        assert!(out.contains("nothing supersedes this memory"));
        assert!(out.contains("supersedes nothing"));
        assert!(out.contains("▶ abc123"));
        assert!(
            !out.contains("NOTE (epoch honesty)"),
            "no caveat when unbounded era"
        );
    }

    #[test]
    fn render_lineage_renders_both_chains_in_order() {
        let lineage = serde_json::json!({
            "id": "mid",
            "start": node("mid", "Middle", Some(200), false),
            "supersedes_chain": [node("old", "Old", Some(100), false)],
            "superseded_by_chain": [node("new", "New", Some(300), false)],
        });
        let out = render_lineage(&lineage, 0);
        let old_pos = out.find("old  Old").unwrap();
        let mid_pos = out.find("▶ mid").unwrap();
        let new_pos = out.find("new  New").unwrap();
        assert!(
            new_pos < mid_pos,
            "newer-than entries render above the anchor"
        );
        assert!(
            mid_pos < old_pos,
            "older-than entries render below the anchor"
        );
    }

    /// invariant-adjacent (MI-W2.4c EPOCH HONESTY): a chain reaching back
    /// before the tombstone era MUST print the caveat; one that doesn't
    /// must NOT (no false alarms).
    #[test]
    fn render_lineage_epoch_honesty_caveat_fires_only_when_window_predates_era() {
        let lineage = serde_json::json!({
            "id": "mid",
            "start": node("mid", "Middle", Some(500), false),
            "supersedes_chain": [node("old", "Old", Some(100), false)],
            "superseded_by_chain": [],
        });
        // Era started AFTER the oldest node (100) → caveat fires, naming
        // both boundary numbers.
        let with_caveat = render_lineage(&lineage, 300);
        assert!(with_caveat.contains("NOTE (epoch honesty)"));
        assert!(with_caveat.contains("100 unix"));
        assert!(with_caveat.contains("300 unix"));

        // Era started AT-OR-BEFORE the oldest node → no caveat; the whole
        // walked window is inside the honest era.
        let no_caveat = render_lineage(&lineage, 100);
        assert!(!no_caveat.contains("NOTE (epoch honesty)"));
        let no_caveat_earlier = render_lineage(&lineage, 50);
        assert!(!no_caveat_earlier.contains("NOTE (epoch honesty)"));
    }

    // CT-B2 — `kb memory recalled-by`.

    #[test]
    fn render_recalled_by_empty_says_so() {
        let body = serde_json::json!({ "rows": [] });
        let out = render_recalled_by(&body);
        assert!(out.contains("no recalls the capture pipeline saw"));
    }

    #[test]
    fn render_recalled_by_renders_title_short_id_and_turn_marker() {
        let body = serde_json::json!({
            "rows": [
                {
                    "session_kb": "sessions",
                    "session_id": "abcdefgh-1234-5678",
                    "session_title": "Fix the reconcile bug",
                    "turn_id": "t-0123456789ab",
                    "recalled_at": 1_700_000_000,
                },
            ],
        });
        let out = render_recalled_by(&body);
        assert!(out.contains("1 recall the capture pipeline saw"));
        assert!(out.contains("recalled in Fix the reconcile bug (abcdefgh) at"));
        assert!(out.contains("[turn t-0123456789ab]"));
    }

    /// MR1 (V0041) — a `pos` on the wire renders as a `[#N]` rank marker,
    /// and its ABSENCE renders as nothing at all (a pre-MR1 row, or a hit
    /// only the free-text fallback grammar parsed). Never `[#?]`, never a
    /// guessed rank.
    #[test]
    fn render_recalled_by_renders_the_pos_rank_marker_and_omits_it_when_absent() {
        let body = serde_json::json!({
            "rows": [
                {
                    "session_kb": "sessions",
                    "session_id": "abcdefgh-1234",
                    "session_title": "Ranked first",
                    "recalled_at": 1_700_000_000,
                    "pos": 1,
                },
                {
                    "session_kb": "sessions",
                    "session_id": "ijklmnop-5678",
                    "session_title": "Pre-MR1 capture",
                    "recalled_at": 1_700_000_100,
                },
            ],
        });
        let out = render_recalled_by(&body);
        assert!(out.contains("recalled in Ranked first (abcdefgh) at"));
        assert!(out.contains("[#1]"), "pos renders as a rank marker: {out}");
        let pre_mr1 = out
            .lines()
            .find(|l| l.contains("Pre-MR1 capture"))
            .expect("second row rendered");
        assert!(
            !pre_mr1.contains("[#"),
            "an absent pos renders nothing at all: {pre_mr1}"
        );
    }

    #[test]
    fn render_recalled_by_falls_back_to_short_id_when_no_title() {
        let body = serde_json::json!({
            "rows": [
                {
                    "session_kb": "sessions",
                    "session_id": "zzzzzzzz-1111",
                    "recalled_at": serde_json::Value::Null,
                },
            ],
        });
        let out = render_recalled_by(&body);
        assert!(out.contains("recalled in zzzzzzzz (zzzzzzzz) at"));
        assert!(
            !out.contains("[turn"),
            "no turn marker when turn_id is absent"
        );
    }

    /// CT-C5 — the `· referenced` suffix renders iff the wire row carries
    /// `used: true` (absent-when-false, so a pre-C5 daemon's rows render
    /// byte-identically to before).
    #[test]
    fn render_recalled_by_marks_used_rows_referenced() {
        let body = serde_json::json!({
            "rows": [
                { "session_id": "aaaaaaaa-1", "used": true },
                { "session_id": "bbbbbbbb-2" },
            ],
        });
        let out = render_recalled_by(&body);
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[1].contains("· referenced"));
        assert!(!lines[2].contains("referenced"));
    }

    // MI-W2.1/2.2 — invariant:10 decomposition, scoring_v2 extension.
    #[test]
    fn explain_line_appends_relevance_and_stability_when_present() {
        let h = serde_json::json!({
            "rank": 2, "rel": 0.032_258_064_5, "salience": 0.5,
            "decay": 0.904_837_4, "age_days": 10.0, "score": 0.014_598_7,
            "relevance_factor": 0.8, "stability": 1.25,
        });
        assert_eq!(
            explain_line(&h),
            "rel(1/(60+2))=0.0323 × salience=0.50 × decay(age=10.0d)=0.9048 × relevance=0.80 × stability=1.25  →  0.0146"
        );
    }

    // ---- repo_slug ----------------------------------------------------

    #[test]
    fn repo_slug_leaves_a_simple_lowercase_name_alone() {
        assert_eq!(repo_slug(Path::new("/home/user/project/kb")), "kb");
    }

    #[test]
    fn repo_slug_lowercases_and_collapses_underscores() {
        assert_eq!(repo_slug(Path::new("/home/user/project/My_App")), "my-app");
    }

    // ---- B1 — main_root_from_common_dir --------------------------------

    #[test]
    fn main_root_from_common_dir_walks_up_from_the_conventional_layout() {
        assert_eq!(
            main_root_from_common_dir("/home/x/proj/.git"),
            Some(std::path::PathBuf::from("/home/x/proj"))
        );
    }

    #[test]
    fn main_root_from_common_dir_trims_the_trailing_newline_git_appends() {
        assert_eq!(
            main_root_from_common_dir("/home/x/proj/.git\n"),
            Some(std::path::PathBuf::from("/home/x/proj"))
        );
    }

    #[test]
    fn main_root_from_common_dir_refuses_a_bare_repo_path() {
        // A bare repo's `--git-common-dir` is the repo dir itself, which
        // doesn't end in `/.git` — nothing to walk up to.
        assert_eq!(main_root_from_common_dir("/home/x/bare.git"), None);
    }

    #[test]
    fn main_root_from_common_dir_refuses_a_bare_dot_git_with_no_parent() {
        assert_eq!(main_root_from_common_dir(".git"), None);
        assert_eq!(main_root_from_common_dir("/.git"), None);
    }

    // ---- B1 — resolve_recall_wire ---------------------------------------

    #[test]
    fn resolve_recall_wire_auto_with_explicit_project_behaves_like_all_plus_project() {
        assert_eq!(
            resolve_recall_wire("auto", Some("some-corpus".to_string()), None),
            ("all".to_string(), Some("some-corpus".to_string()), None)
        );
        // An explicit --project wins even when a slug WOULD have resolved —
        // the caller named a target, so there's nothing to guess.
        assert_eq!(
            resolve_recall_wire(
                "auto",
                Some("some-corpus".to_string()),
                Some("kb".to_string())
            ),
            ("all".to_string(), Some("some-corpus".to_string()), None)
        );
    }

    #[test]
    fn resolve_recall_wire_auto_with_a_slug_narrows_to_the_project_corpus() {
        assert_eq!(
            resolve_recall_wire("auto", None, Some("kb".to_string())),
            (
                "all".to_string(),
                Some("memory-kb".to_string()),
                Some("kb,memory-kb".to_string())
            )
        );
    }

    #[test]
    fn resolve_recall_wire_auto_with_no_slug_degrades_to_todays_default() {
        assert_eq!(
            resolve_recall_wire("auto", None, None),
            ("all".to_string(), None, None)
        );
    }

    #[test]
    fn resolve_recall_wire_non_auto_scopes_pass_through_byte_identical() {
        // Every non-auto scope must be a pure passthrough — never any
        // visible_to, project untouched — regardless of whether a slug
        // happened to resolve (recall_inner never even derives one).
        for scope in ["all", "global", "project"] {
            assert_eq!(
                resolve_recall_wire(scope, None, Some("kb".to_string())),
                (scope.to_string(), None, None)
            );
            assert_eq!(
                resolve_recall_wire(scope, Some("named".to_string()), Some("kb".to_string())),
                (scope.to_string(), Some("named".to_string()), None)
            );
        }
    }

    // ---- CT-C3 — apply_failed_outcome (--failed wiring) ----------------

    #[test]
    fn apply_failed_outcome_off_is_a_byte_identical_noop() {
        let mut tags = vec!["rust".to_string()];
        assert_eq!(apply_failed_outcome(false, &mut tags), None);
        assert_eq!(tags, vec!["rust".to_string()]);
    }

    #[test]
    fn apply_failed_outcome_appends_the_tag_once_and_returns_failed() {
        let mut tags = vec!["rust".to_string()];
        assert_eq!(apply_failed_outcome(true, &mut tags), Some("failed"));
        assert_eq!(
            tags,
            vec![
                "rust".to_string(),
                kb_core::memory::FAILED_OUTCOME_TAG.to_string()
            ]
        );
        // Idempotent — a user who already typed --tags outcome:failed (or
        // the slug spelling) never gets a duplicate.
        assert_eq!(apply_failed_outcome(true, &mut tags), Some("failed"));
        assert_eq!(tags.len(), 2);
        let mut slugged = vec!["outcome-failed".to_string()];
        assert_eq!(apply_failed_outcome(true, &mut slugged), Some("failed"));
        assert_eq!(slugged.len(), 1);
    }

    // ---- MI-W3.4 — resolve_visibility ----------------------------------

    #[test]
    fn resolve_visibility_defaults_to_global_absent_everything() {
        assert_eq!(
            resolve_visibility(None, false, None, None),
            (true, Vec::new())
        );
    }

    #[test]
    fn resolve_visibility_link_wins_outright() {
        assert_eq!(
            resolve_visibility(Some("a, b"), false, None, None),
            (false, vec!["a".to_string(), "b".to_string()])
        );
        // Even with an untrusted source and no --global — link still wins.
        assert_eq!(
            resolve_visibility(Some("a"), false, None, Some("fetched-web")),
            (false, vec!["a".to_string()])
        );
    }

    #[test]
    fn resolve_visibility_explicit_global_wins_over_the_untrusted_gate() {
        assert_eq!(
            resolve_visibility(None, true, None, Some("fetched-web")),
            (true, Vec::new()),
            "--global must win even for an untrusted source"
        );
    }

    #[test]
    fn resolve_visibility_auto_link_wins_over_the_untrusted_gate() {
        // A resolvable project kb means the write was never going to be
        // global anyway — the gate is moot, not double-applied.
        assert_eq!(
            resolve_visibility(None, false, Some("myproj".to_string()), Some("fetched-web")),
            (false, vec!["myproj".to_string()])
        );
    }

    #[test]
    fn resolve_visibility_untrusted_web_source_flips_the_bare_default() {
        assert_eq!(
            resolve_visibility(None, false, None, Some("fetched-web")),
            (false, Vec::new()),
            "fetched-web with nothing else set must default to non-global"
        );
    }

    #[test]
    fn resolve_visibility_trusted_sources_do_not_flip_the_default() {
        assert_eq!(
            resolve_visibility(None, false, None, Some("user-dictated")),
            (true, Vec::new())
        );
        assert_eq!(
            resolve_visibility(None, false, None, Some("agent-inference")),
            (true, Vec::new())
        );
    }

    #[test]
    fn resolve_visibility_unrecognised_source_is_not_treated_as_untrusted() {
        // Defensive re-parse: an invalid value (the caller already
        // rejected it before this point in the real CLI path) must not be
        // misread as the untrusted case.
        assert_eq!(
            resolve_visibility(None, false, None, Some("not-a-real-source")),
            (true, Vec::new())
        );
    }

    // ---- pick_memory_target --------------------------------------------

    #[test]
    fn pick_memory_target_prefers_the_named_project_corpus() {
        let kbs = serde_json::json!([
            {"name": "memory-kb", "memory_scope": "project"},
            {"name": "memory", "memory_scope": "global"},
            {"name": "kb", "memory_scope": null},
        ]);
        let target = pick_memory_target(&kbs, "project", "kb").unwrap();
        assert_eq!(target, MemoryTarget::Corpus("memory-kb".to_string()));
    }

    #[test]
    fn pick_memory_target_auto_links_the_global_corpus_when_the_project_kb_exists() {
        let kbs = serde_json::json!([
            {"name": "memory", "memory_scope": "global"},
            {"name": "kb", "memory_scope": null},
            {"name": "some-other-project", "memory_scope": null},
        ]);
        // No "memory-kb" corpus, so falls back to the global corpus, but
        // since a "kb" kb exists it auto-links to it instead of going fully
        // global.
        let target = pick_memory_target(&kbs, "project", "kb").unwrap();
        assert_eq!(
            target,
            MemoryTarget::GlobalLinked {
                kb: "memory".to_string(),
                link: Some("kb".to_string()),
            }
        );
    }

    #[test]
    fn pick_memory_target_falls_back_to_plain_global_when_no_matching_kb_exists() {
        let kbs = serde_json::json!([
            {"name": "memory", "memory_scope": "global"},
            {"name": "some-other-project", "memory_scope": null},
        ]);
        // No "memory-kb" corpus AND no kb literally named "kb" — nothing to
        // auto-link to, so it's a plain global write.
        let target = pick_memory_target(&kbs, "project", "kb").unwrap();
        assert_eq!(
            target,
            MemoryTarget::GlobalLinked {
                kb: "memory".to_string(),
                link: None,
            }
        );
    }

    #[test]
    fn pick_memory_target_errors_when_neither_project_nor_global_corpus_exists() {
        let kbs = serde_json::json!([{"name": "kb", "memory_scope": null}]);
        let err = pick_memory_target(&kbs, "project", "kb").unwrap_err();
        assert!(err.to_string().contains("memory-kb"));
        assert!(err.to_string().contains("global"));
    }

    #[test]
    fn pick_memory_target_global_scope_happy_path() {
        let kbs = serde_json::json!([
            {"name": "memory", "memory_scope": "global"},
            {"name": "memory-kb", "memory_scope": "project"},
        ]);
        let target = pick_memory_target(&kbs, "global", "kb").unwrap();
        assert_eq!(target, MemoryTarget::Corpus("memory".to_string()));
    }

    #[test]
    fn pick_memory_target_global_scope_zero_matches_errors() {
        let kbs = serde_json::json!([{"name": "memory-kb", "memory_scope": "project"}]);
        let err = pick_memory_target(&kbs, "global", "kb").unwrap_err();
        assert!(err.to_string().contains("memory_scope=\"global\""));
    }

    #[test]
    fn pick_memory_target_global_scope_multi_match_errors() {
        let kbs = serde_json::json!([
            {"name": "memory-a", "memory_scope": "global"},
            {"name": "memory-b", "memory_scope": "global"},
        ]);
        let err = pick_memory_target(&kbs, "global", "kb").unwrap_err();
        assert!(err.to_string().contains("memory-a"));
        assert!(err.to_string().contains("memory-b"));
    }

    #[test]
    fn pick_memory_target_unknown_scope_surfaces_the_zero_match_error() {
        let kbs = serde_json::json!([
            {"name": "memory", "memory_scope": "global"},
            {"name": "memory-kb", "memory_scope": "project"},
        ]);
        let err = pick_memory_target(&kbs, "bogus-scope", "kb").unwrap_err();
        assert!(err.to_string().contains("memory_scope=\"bogus-scope\""));
    }

    // ---- pick_global_write_target / the sessions-corpus regression ------
    //
    // Live fleet shape: two corpora carry memory_scope="global" — "memory"
    // (curated) and "sessions" (captured transcripts, tagged global only so
    // its rows join the recall fan-out). Both write paths must disambiguate
    // to "memory" instead of hard-erroring on the 2-match case.

    #[test]
    fn pick_memory_target_project_fallback_prefers_memory_over_sessions_with_link() {
        let kbs = serde_json::json!([
            {"name": "memory", "memory_scope": "global"},
            {"name": "sessions", "memory_scope": "global"},
            {"name": "kb", "memory_scope": null},
        ]);
        // No "memory-kb" project corpus, so falls back to the global
        // candidates {"memory","sessions"} — "memory" must win over
        // "sessions", and since a "kb" kb exists it still auto-links.
        let target = pick_memory_target(&kbs, "project", "kb").unwrap();
        assert_eq!(
            target,
            MemoryTarget::GlobalLinked {
                kb: "memory".to_string(),
                link: Some("kb".to_string()),
            }
        );
    }

    #[test]
    fn pick_memory_target_project_fallback_prefers_memory_over_sessions_no_link() {
        let kbs = serde_json::json!([
            {"name": "memory", "memory_scope": "global"},
            {"name": "sessions", "memory_scope": "global"},
            {"name": "some-other-project", "memory_scope": null},
        ]);
        // Same disambiguation, but no kb named "kb" exists — plain global
        // write, no auto-link.
        let target = pick_memory_target(&kbs, "project", "kb").unwrap();
        assert_eq!(
            target,
            MemoryTarget::GlobalLinked {
                kb: "memory".to_string(),
                link: None,
            }
        );
    }

    #[test]
    fn pick_memory_target_global_scope_prefers_memory_over_sessions() {
        let kbs = serde_json::json!([
            {"name": "memory", "memory_scope": "global"},
            {"name": "sessions", "memory_scope": "global"},
        ]);
        let target = pick_memory_target(&kbs, "global", "kb").unwrap();
        assert_eq!(target, MemoryTarget::Corpus("memory".to_string()));
    }

    #[test]
    fn pick_memory_target_global_scope_single_non_memory_named_corpus_still_resolves() {
        // No name requirement when unambiguous — a lone global corpus wins
        // regardless of what it's called.
        let kbs = serde_json::json!([{"name": "mem-global", "memory_scope": "global"}]);
        let target = pick_memory_target(&kbs, "global", "kb").unwrap();
        assert_eq!(target, MemoryTarget::Corpus("mem-global".to_string()));
    }

    #[test]
    fn pick_global_write_target_prefers_the_conventional_name() {
        assert_eq!(
            pick_global_write_target(&["sessions", "memory"]).unwrap(),
            "memory"
        );
    }

    #[test]
    fn pick_global_write_target_single_candidate_needs_no_name_match() {
        assert_eq!(
            pick_global_write_target(&["mem-global"]).unwrap(),
            "mem-global"
        );
    }

    #[test]
    fn pick_global_write_target_ambiguous_without_memory_errors() {
        let err = pick_global_write_target(&["memory-a", "memory-b"]).unwrap_err();
        assert!(err.to_string().contains("memory-a"));
        assert!(err.to_string().contains("memory-b"));
    }

    #[test]
    fn pick_global_write_target_empty_errors() {
        let err = pick_global_write_target(&[]).unwrap_err();
        assert!(err.to_string().contains("memory_scope=\"global\""));
    }

    // ---- MI-W3.1 — `kb memory dupes` render ---------------------------

    #[test]
    fn render_dupes_reports_zero_pairs_honestly() {
        let body = serde_json::json!({"pairs": [], "threshold": 0.9, "scanned": 372});
        let out = render_dupes(&body);
        assert!(out.contains("372 candidate memories scanned"));
        assert!(out.contains("threshold 0.90"));
        assert!(out.contains("no likely-redundant pairs found"));
    }

    #[test]
    fn render_dupes_renders_pairs_with_cross_corpus_tag() {
        let body = serde_json::json!({
            "pairs": [
                {"kb_a": "memory", "id_a": "aaa", "title_a": "T A",
                 "kb_b": "memory-kb", "id_b": "bbb", "title_b": "T B",
                 "cosine": 0.95, "cross_corpus": true},
                {"kb_a": "memory", "id_a": "ccc", "title_a": "T C",
                 "kb_b": "memory", "id_b": "ddd", "title_b": "T D",
                 "cosine": 0.91, "cross_corpus": false},
            ],
            "threshold": 0.9,
            "scanned": 2,
        });
        let out = render_dupes(&body);
        assert!(out.contains("2 candidate memories scanned"));
        assert!(out.contains("[cross-corpus]"));
        assert!(out.contains("0.950"));
        assert!(out.contains("T A"));
        assert!(out.contains("T B"));
        // The same-corpus pair's own header line must NOT carry the tag.
        let same_corpus_header = out
            .lines()
            .find(|l| l.starts_with("0.910"))
            .expect("second pair's cosine header line");
        assert!(!same_corpus_header.contains("[cross-corpus]"));
    }

    // ---- MI-W4.4 — `kb memory triage` render --------------------------

    #[test]
    fn render_triage_reports_an_empty_queue_honestly() {
        let body = serde_json::json!({"items": [], "scanned": 120});
        let out = render_triage(&body);
        assert!(out.contains("120 memories scanned"));
        assert!(out.contains("0 in the queue"));
        assert!(out.contains("queue is empty"));
    }

    #[test]
    fn render_triage_renders_items_with_urgency_and_reason() {
        let body = serde_json::json!({
            "items": [
                {"kb": "globalmem", "id": "aaaaaaaaaaaa", "title": "Old fact",
                 "reason_kind": "below_floor_now",
                 "reason": "salience 0.10 is at/below the 0.15 floor — excluded from recall now",
                 "urgency": 0.8, "salience": 0.10, "floor": 0.15},
                {"kb": "globalmem", "id": "bbbbbbbbbbbb", "title": "Hot but unused",
                 "reason_kind": "high_salience_dormant",
                 "reason": "salience 0.90 but never recalled",
                 "urgency": 0.9, "salience": 0.9},
            ],
            "scanned": 42,
        });
        let out = render_triage(&body);
        assert!(out.contains("42 memories scanned"));
        assert!(out.contains("2 in the queue"));
        assert!(out.contains("0.80"));
        assert!(out.contains("Old fact"));
        assert!(out.contains("salience 0.10 is at/below the 0.15 floor — excluded from recall now"));
        assert!(out.contains("0.90"));
        assert!(out.contains("Hot but unused"));
        assert!(out.contains("salience 0.90 but never recalled"));
    }

    #[test]
    fn render_triage_singular_scanned_count() {
        let body = serde_json::json!({"items": [], "scanned": 1});
        let out = render_triage(&body);
        assert!(out.contains("1 memory scanned"), "out: {out:?}");
    }

    // ---- CT-B4 — `kb memory expand` ------------------------------------

    #[test]
    fn meta_content_round_trips_a_real_render_artifact_output() {
        // Exercises the ACTUAL writer (`kb_core::memory::render_artifact`)
        // rather than a hand-written fixture, so this stays honest about
        // what `meta_content` really has to parse.
        let anchor = kb_core::review::Anchor::Selection {
            css_path: "main > p".into(),
            offset: 3,
            snippet: "quoted \"text\" & <tags>".into(),
        };
        let provenance = kb_core::memory::MemoryProvenance {
            author: None,
            source_kb: Some("kb-docs".into()),
            source_artifact: Some("a1b2c3d4e5f6".into()),
            source_anchor: Some(anchor.clone()),
            source: None,
        };
        let html = kb_core::memory::render_artifact(
            "A memory",
            "<p>body</p>",
            "memory",
            &[],
            None,
            None,
            None,
            None,
            false,
            &[],
            None,
            None,
            Some(&provenance),
            None,
            None,
        );

        assert_eq!(
            meta_content(&html, "kb-source-kb").as_deref(),
            Some("kb-docs")
        );
        assert_eq!(
            meta_content(&html, "kb-source-artifact").as_deref(),
            Some("a1b2c3d4e5f6")
        );
        let raw_anchor = meta_content(&html, "kb-source-anchor").expect("anchor meta present");
        let parsed: kb_core::review::Anchor =
            serde_json::from_str(&raw_anchor).expect("valid Anchor JSON");
        assert_eq!(parsed, anchor);
    }

    #[test]
    fn meta_content_is_none_for_an_ordinary_non_highlight_born_memory() {
        let html = kb_core::memory::render_artifact(
            "Plain memory",
            "<p>body</p>",
            "memory",
            &[],
            None,
            None,
            None,
            None,
            false,
            &[],
            None,
            None,
            None,
            None,
            None,
        );
        assert_eq!(meta_content(&html, "kb-source-kb"), None);
        assert_eq!(meta_content(&html, "kb-source-artifact"), None);
        assert_eq!(meta_content(&html, "kb-source-anchor"), None);
    }

    #[test]
    fn unescape_attr_inverts_the_amp_lt_gt_quot_escaping() {
        assert_eq!(
            unescape_attr("quoted &quot;text&quot; &amp; &lt;tags&gt;"),
            "quoted \"text\" & <tags>"
        );
        // A literal "&lt;" in the ORIGINAL text is escaped to "&amp;lt;" by
        // `escape_attr` (it escapes `&` first) — must decode back to the
        // literal "&lt;", not accidentally collapse further to "<".
        assert_eq!(unescape_attr("&amp;lt;"), "&lt;");
    }

    const PASSAGE_HTML: &str = r#"<!doctype html><html><head><title>t</title></head><body>
<h1 id="top">Title here</h1>
<p>intro paragraph</p>
<h2 id="alpha">Alpha section</h2>
<p>alpha body text</p>
<h2 id="beta">Beta section</h2>
<p>beta body text</p>
</body></html>"#;

    #[test]
    fn passage_for_section_returns_the_section_body_when_resolved() {
        let anchor = kb_core::review::Anchor::Section {
            id: "alpha".into(),
            tag: None,
            snippet: None,
        };
        let resolution = kb_core::review::fuzzy_resolve_anchor(PASSAGE_HTML, &anchor);
        assert_eq!(
            resolution,
            kb_core::review::Resolution::Exact("alpha".into())
        );
        let passage = passage_for(PASSAGE_HTML, &anchor, &resolution).expect("resolves");
        assert!(passage.contains("Alpha section"));
        assert!(passage.contains("alpha body text"));
        assert!(!passage.contains("Beta section"));
    }

    #[test]
    fn passage_for_section_is_none_when_the_anchor_no_longer_resolves() {
        let anchor = kb_core::review::Anchor::Section {
            id: "gone".into(),
            tag: None,
            snippet: None,
        };
        let resolution = kb_core::review::fuzzy_resolve_anchor(PASSAGE_HTML, &anchor);
        assert_eq!(resolution, kb_core::review::Resolution::Stale);
        assert_eq!(passage_for(PASSAGE_HTML, &anchor, &resolution), None);
    }

    #[test]
    fn passage_for_selection_uses_the_resolutions_own_matched_text() {
        let anchor = kb_core::review::Anchor::Selection {
            css_path: "p".into(),
            offset: 0,
            snippet: "alpha body text".into(),
        };
        let resolution = kb_core::review::fuzzy_resolve_anchor(PASSAGE_HTML, &anchor);
        let passage = passage_for(PASSAGE_HTML, &anchor, &resolution).expect("resolves");
        assert_eq!(passage, "alpha body text");
    }

    #[test]
    fn passage_for_file_is_always_none() {
        let resolution =
            kb_core::review::fuzzy_resolve_anchor(PASSAGE_HTML, &kb_core::review::Anchor::File);
        assert_eq!(
            passage_for(PASSAGE_HTML, &kb_core::review::Anchor::File, &resolution),
            None
        );
    }

    #[test]
    fn cap_selection_passage_collapses_whitespace_and_truncates() {
        assert_eq!(cap_selection_passage("  a   b\nc  "), "a b c");
        let long = "word ".repeat(2000);
        let capped = cap_selection_passage(long.trim());
        assert!(capped.ends_with('…'));
        assert!(capped.chars().count() <= kb_core::lists::SECTION_PASSAGE_CHAR_CAP + 1);
    }

    #[test]
    fn anchor_header_renders_every_scope() {
        assert_eq!(
            anchor_header(&serde_json::json!({"kind":"section","id":"alpha"})),
            "  — section \"alpha\""
        );
        assert_eq!(
            anchor_header(&serde_json::json!({"kind":"chapter","path":"Top > Leaf"})),
            "  — Top > Leaf"
        );
        assert_eq!(
            anchor_header(&serde_json::json!({"kind":"selection"})),
            "  — selection"
        );
        assert_eq!(
            anchor_header(&serde_json::json!({"kind":"file"})),
            "  — whole artifact"
        );
        assert_eq!(anchor_header(&serde_json::Value::Null), "");
    }

    #[test]
    fn render_expand_no_origin_recorded_says_so_honestly() {
        let out = serde_json::json!({
            "memory": {"id": "abc123", "kb": "memory", "title": "An ordinary fact"},
            "source": null,
            "anchor": null,
            "resolved": false,
            "passage": null,
        });
        let text = render_expand(&out);
        assert!(text.contains("An ordinary fact"));
        assert!(text.contains("not a highlight-born memory — no origin recorded"));
    }

    #[test]
    fn render_expand_origin_artifact_gone_says_so_honestly() {
        let out = serde_json::json!({
            "memory": {"id": "abc123", "kb": "memory", "title": "A highlight"},
            "source": {"kb": "kb-docs", "artifact": "deadbeefcafe", "title": null},
            "anchor": {"kind": "selection", "css_path": "p", "offset": 0, "snippet": "x"},
            "resolved": false,
            "passage": null,
        });
        let text = render_expand(&out);
        assert!(text.contains("origin artifact no longer exists"));
        assert!(text.contains("kb-docs"));
        assert!(text.contains("deadbeefcafe"));
    }

    #[test]
    fn render_expand_stale_anchor_never_prints_a_guessed_passage() {
        let out = serde_json::json!({
            "memory": {"id": "abc123", "kb": "memory", "title": "A highlight"},
            "source": {"kb": "kb-docs", "artifact": "deadbeefcafe", "title": "The Origin Doc"},
            "anchor": {"kind": "section", "id": "alpha", "tag": null, "snippet": null},
            "resolved": false,
            "passage": null,
        });
        let text = render_expand(&out);
        assert!(
            text.contains("anchor no longer resolves — the source has changed since the highlight")
        );
        assert!(text.contains("The Origin Doc"));
        assert!(text.contains("kb cat deadbeefcafe --kb kb-docs"));
    }

    #[test]
    fn render_expand_resolved_prints_the_header_and_passage() {
        let out = serde_json::json!({
            "memory": {"id": "abc123", "kb": "memory", "title": "A highlight"},
            "source": {"kb": "kb-docs", "artifact": "deadbeefcafe", "title": "The Origin Doc"},
            "anchor": {"kind": "section", "id": "alpha", "tag": null, "snippet": null},
            "resolved": true,
            "passage": "Alpha section alpha body text",
        });
        let text = render_expand(&out);
        assert!(text.contains("from: The Origin Doc  — section \"alpha\""));
        assert!(text.contains("Alpha section alpha body text"));
    }

    #[test]
    fn render_expand_resolved_with_no_passage_says_whole_artifact() {
        let out = serde_json::json!({
            "memory": {"id": "abc123", "kb": "memory", "title": "A highlight"},
            "source": {"kb": "kb-docs", "artifact": "deadbeefcafe", "title": "The Origin Doc"},
            "anchor": serde_json::Value::Null,
            "resolved": true,
            "passage": null,
        });
        let text = render_expand(&out);
        assert!(text.contains("no specific passage — this highlight anchors the whole artifact"));
    }
}
