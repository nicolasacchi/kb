//! `kb-code inbox` (S2-A CLI, `/tmp/design-s2.md` § S2-A) — one-shot render
//! / `--watch` poll loop over `GET /api/inbox` (`unified-inbox/1`,
//! kb-code-server's federated three-lane attention queue: reviews awaiting
//! you, open questions in working trees, and kb's desk + comments lanes).
//!
//! Deliberately **NOT** wired into `watch.rs`: that module's `SeenKey`/
//! `WatchScope` model is an SSE-driven, single-daemon design (`annotate
//! watch`) and stays untouched. `unified-inbox/1` has no SSE bus of its
//! own, so this is a separate, simpler HTTP POLL loop with its OWN
//! seed-then-diff seen-set, keyed on the EXACT tuples the design doc pins:
//!   - review rows:      `(review_id, updated_at, score)`
//!   - annotation rows:  `(id, updated_at)`
//!   - kb comment items: `(kb, comment_id, updated_at)`
//!   - kb desk items:    `(kb, id, updated_unix)`
//!
//! `score` never rides the wire row — the `reviews` lane is verbatim
//! `review-inbox/1` JSON (the SAME shape the per-repo `kb-code review
//! inbox` route already emits — see `review_inbox_cmd` in `main.rs`), so
//! [`review_score`] recomputes it here from the two fields that ARE on the
//! row, using the SAME closed formula kb-code-server's own
//! `review_inbox::inbox_score` uses server-side:
//! `unanswered_questions*2 + unresolved_findings`.
//!
//! Every extraction/diff/render fn below is pure (`serde_json::Value` in,
//! no I/O) so it's unit-testable over fixture JSON with no daemon involved
//! — `run_once`/`run_watch` are thin HTTP shells around them.

use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::HashSet;
use std::time::Duration;
use tokio::time::Instant;

pub const SCHEMA: &str = "unified-inbox/1";

/// Dedupe key for one row, namespaced per lane so ids never collide across
/// lanes — the exact tuples pinned in the module doc. A changed field
/// (most commonly `updated_at`) mints a NEW key, so [`take_new`] treats it
/// as a fresh "new/changed" surfacing — mirrors `watch.rs::SeenKey`'s own
/// "any field change is a new item" posture.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum InboxSeenKey {
    Review {
        review_id: i64,
        updated_at: i64,
        score: i64,
    },
    Annotation {
        id: String,
        updated_at: i64,
    },
    KbComment {
        kb: String,
        comment_id: String,
        updated_at: i64,
    },
    KbDesk {
        kb: String,
        id: String,
        updated_unix: i64,
    },
}

/// Which of the (up to) four lanes an [`InboxRow`] came from — drives
/// which `format_*_row` fn renders it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lane {
    Review,
    Annotation,
    KbDesk,
    KbComment,
}

/// One row plus its dedupe key and lane tag — the diffable unit
/// [`take_new`]/[`seed_seen`] operate on.
#[derive(Clone, Debug, PartialEq)]
pub struct InboxRow {
    pub lane: Lane,
    pub key: InboxSeenKey,
    pub value: Value,
}

// --- pure extraction -------------------------------------------------------

/// `score = unanswered_questions*2 + unresolved_findings` — the SAME
/// closed formula `kb_code_server::review_inbox::inbox_score` computes
/// server-side (duplicated here rather than imported: this crate doesn't
/// depend on kb-code-server's private module surface for this, and the
/// two fields it needs already ride the wire row verbatim).
fn review_score(row: &Value) -> i64 {
    let unanswered = row["unanswered_questions"].as_i64().unwrap_or(0);
    let unresolved = row["unresolved_findings"].as_i64().unwrap_or(0);
    unanswered * 2 + unresolved
}

fn review_row(row: &Value) -> Option<InboxRow> {
    let review_id = row["review_id"].as_i64()?;
    let updated_at = row["updated_at"].as_i64().unwrap_or(0);
    let score = review_score(row);
    Some(InboxRow {
        lane: Lane::Review,
        key: InboxSeenKey::Review {
            review_id,
            updated_at,
            score,
        },
        value: row.clone(),
    })
}

fn annotation_row(row: &Value) -> Option<InboxRow> {
    let id = row["id"].as_str()?.to_string();
    let updated_at = row["updated_at"].as_i64().unwrap_or(0);
    Some(InboxRow {
        lane: Lane::Annotation,
        key: InboxSeenKey::Annotation { id, updated_at },
        value: row.clone(),
    })
}

fn kb_desk_row(row: &Value) -> Option<InboxRow> {
    let kb = row["kb"].as_str()?.to_string();
    let id = row["id"].as_str()?.to_string();
    let updated_unix = row["updated_unix"].as_i64().unwrap_or(0);
    Some(InboxRow {
        lane: Lane::KbDesk,
        key: InboxSeenKey::KbDesk {
            kb,
            id,
            updated_unix,
        },
        value: row.clone(),
    })
}

fn kb_comment_row(row: &Value) -> Option<InboxRow> {
    let kb = row["kb"].as_str()?.to_string();
    let comment_id = row["comment_id"].as_str()?.to_string();
    let updated_at = row["updated_at"].as_i64().unwrap_or(0);
    Some(InboxRow {
        lane: Lane::KbComment,
        key: InboxSeenKey::KbComment {
            kb,
            comment_id,
            updated_at,
        },
        value: row.clone(),
    })
}

/// Flatten every row across all (up to) four lanes into one diffable list,
/// in lane order (reviews, annotations, kb desk, kb comments). The kb
/// sub-arrays are naturally empty when the kb lane is degraded
/// (`kb.available == false`, so `kb.desk`/`kb.comments` are `null`) or
/// simply absent (an older daemon) — indexing a `Value` with a missing/
/// non-object key returns `Value::Null` rather than panicking, so this
/// needs no extra degradation branch of its own.
pub fn rows_from_body(body: &Value) -> Vec<InboxRow> {
    let mut out = Vec::new();
    if let Some(reviews) = body["reviews"].as_array() {
        out.extend(reviews.iter().filter_map(review_row));
    }
    if let Some(anns) = body["annotations"].as_array() {
        out.extend(anns.iter().filter_map(annotation_row));
    }
    if let Some(items) = body["kb"]["desk"]["items"].as_array() {
        out.extend(items.iter().filter_map(kb_desk_row));
    }
    if let Some(items) = body["kb"]["comments"]["items"].as_array() {
        out.extend(items.iter().filter_map(kb_comment_row));
    }
    out
}

/// `body.kb.available` — `None` when the field itself is absent/non-bool
/// (defensive; should never happen against a real daemon, but a hand-built
/// or truncated fixture shouldn't panic the diff loop).
pub fn kb_available(body: &Value) -> Option<bool> {
    body["kb"]["available"].as_bool()
}

/// `body.kb.reason` — closed vocab `"disabled" | "unreachable" |
/// "sibling_mismatch"` server-side; read verbatim (never validated against
/// the vocab here — an unrecognised value still renders honestly rather
/// than being coerced to "unknown").
pub fn kb_reason(body: &Value) -> Option<String> {
    body["kb"]["reason"].as_str().map(str::to_string)
}

// --- pure diff ---------------------------------------------------------

/// Insert every row's key into `seen`; return the ones that were NOT
/// already present (i.e. new or changed since the last call). Mirrors
/// `watch.rs::take_new`.
pub fn take_new(seen: &mut HashSet<InboxSeenKey>, rows: &[InboxRow]) -> Vec<InboxRow> {
    let mut out = Vec::new();
    for row in rows {
        if seen.insert(row.key.clone()) {
            out.push(row.clone());
        }
    }
    out
}

/// Seed `seen` from `rows` without surfacing anything — the "seed" half of
/// "seed-then-diff."
pub fn seed_seen(seen: &mut HashSet<InboxSeenKey>, rows: &[InboxRow]) {
    for row in rows {
        seen.insert(row.key.clone());
    }
}

impl InboxSeenKey {
    /// V71-X1 — the row's own recency timestamp, for [`seed_seen_since`]'s
    /// `since` window. Every variant carries one under a different field
    /// name (`updated_at` or, for the kb-desk lane, `updated_unix`).
    pub fn timestamp(&self) -> i64 {
        match self {
            InboxSeenKey::Review { updated_at, .. }
            | InboxSeenKey::Annotation { updated_at, .. }
            | InboxSeenKey::KbComment { updated_at, .. } => *updated_at,
            InboxSeenKey::KbDesk { updated_unix, .. } => *updated_unix,
        }
    }
}

/// V71-X1 — like [`seed_seen`], but ALSO returns rows whose OWN
/// [`InboxSeenKey::timestamp`] is at or after `since` (or, when `backlog`
/// is set, every row) — `kb-code watch`'s "catch me up since I last
/// watched" seeding. [`seed_seen`] itself stays untouched (the standalone
/// `kb-code inbox --watch` keeps its own always-silent seed, dumping the
/// full snapshot separately instead — see [`run_watch`]'s doc); this is an
/// ADDITIVE sibling, not a fork of that behavior.
pub fn seed_seen_since(
    seen: &mut HashSet<InboxSeenKey>,
    rows: &[InboxRow],
    backlog: bool,
    since: Option<i64>,
) -> Vec<InboxRow> {
    let mut surfaced = Vec::new();
    for row in rows {
        seen.insert(row.key.clone());
        if backlog || since.is_some_and(|s| row.key.timestamp() >= s) {
            surfaced.push(row.clone());
        }
    }
    surfaced
}

// --- pure render ---------------------------------------------------------

/// Reuses the EXACT row grammar `review_inbox_cmd`'s human render prints
/// (`main.rs`) — one line per review, so `kb-code inbox` and `kb-code
/// review inbox` read identically for the reviews lane (design doc: "the
/// reviews lane reuses the inbox row grammar from `review inbox`").
pub fn format_review_row(row: &Value) -> String {
    let pr = match row["pr_number"].as_i64() {
        Some(n) => format!("#{n}"),
        None => "-".to_string(),
    };
    format!(
        "review {:<5} {:10} {pr:<6} unanswered={:<3} unresolved={:<3} drift={}  {}",
        row["review_id"],
        row["repo"].as_str().unwrap_or("?"),
        row["unanswered_questions"],
        row["unresolved_findings"],
        row["pr_head_drift"],
        row["title"].as_str().unwrap_or(""),
    )
}

/// One open working-tree question/flag-for-agent row:
/// `{id, repo, path, line?, intent, author, excerpt, reply_count,
/// updated_at}` (design doc, verbatim).
pub fn format_annotation_row(row: &Value) -> String {
    let path = row["path"].as_str().unwrap_or("?");
    let loc = match row["line"].as_i64() {
        Some(n) => format!("{path}:{n}"),
        None => path.to_string(),
    };
    format!(
        "annotation {:<8} {:<16} {:<24} {:<16} replies={:<3} {}",
        row["id"].as_str().unwrap_or("?"),
        row["repo"].as_str().unwrap_or("?"),
        loc,
        row["intent"].as_str().unwrap_or("note"),
        row["reply_count"].as_i64().unwrap_or(0),
        row["excerpt"].as_str().unwrap_or(""),
    )
}

/// kb desk lane row (`DeskListItem`, kb-server, relayed verbatim).
pub fn format_kb_desk_row(row: &Value) -> String {
    format!(
        "desk    {:<10} {:<14} read={:<12} changed={:<5} {}",
        row["kb"].as_str().unwrap_or("?"),
        row["id"].as_str().unwrap_or("?"),
        row["read_state"].as_str().unwrap_or("?"),
        row["changed_since_read"].as_bool().unwrap_or(false),
        row["title"].as_str().unwrap_or(""),
    )
}

/// kb open-comments lane row (`InboxItem`, kb-server, relayed verbatim).
pub fn format_kb_comment_row(row: &Value) -> String {
    format!(
        "comment {:<10} {:<14} by {:<10} replies={:<3} {}",
        row["kb"].as_str().unwrap_or("?"),
        row["comment_id"].as_str().unwrap_or("?"),
        row["author"].as_str().unwrap_or("?"),
        row["reply_count"].as_i64().unwrap_or(0),
        row["excerpt"].as_str().unwrap_or(""),
    )
}

/// Dispatch one [`InboxRow`] to its lane's formatter — used by the
/// `--watch` loop to print a single new/changed row.
pub fn format_row(row: &InboxRow) -> String {
    match row.lane {
        Lane::Review => format_review_row(&row.value),
        Lane::Annotation => format_annotation_row(&row.value),
        Lane::KbDesk => format_kb_desk_row(&row.value),
        Lane::KbComment => format_kb_comment_row(&row.value),
    }
}

/// Full one-shot human render: three headered lane sections. The kb
/// section renders availability honestly — a degraded lane prints its
/// `reason` rather than an empty section pretending zero (design doc:
/// "never an empty section pretending zero").
pub fn format_body_human(body: &Value) -> String {
    let mut out = String::new();

    let reviews = body["reviews"].as_array().cloned().unwrap_or_default();
    out.push_str(&format!("Reviews awaiting you ({})\n", reviews.len()));
    if reviews.is_empty() {
        out.push_str("  (none)\n");
    }
    for r in &reviews {
        out.push_str("  ");
        out.push_str(&format_review_row(r));
        out.push('\n');
    }

    let anns = body["annotations"].as_array().cloned().unwrap_or_default();
    out.push_str(&format!(
        "\nOpen questions in working trees ({})\n",
        anns.len()
    ));
    if anns.is_empty() {
        out.push_str("  (none)\n");
    }
    for a in &anns {
        out.push_str("  ");
        out.push_str(&format_annotation_row(a));
        out.push('\n');
    }

    out.push_str("\nFrom kb\n");
    match kb_available(body) {
        Some(true) => {
            let desk_items = body["kb"]["desk"]["items"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let desk_attention = body["kb"]["desk"]["attention"].as_u64().unwrap_or(0);
            let desk_truncated = body["kb"]["desk"]["truncated"].as_bool().unwrap_or(false);
            out.push_str(&format!(
                "  desk: attention={desk_attention} items={}{}\n",
                desk_items.len(),
                if desk_truncated { " (truncated)" } else { "" },
            ));
            for d in &desk_items {
                out.push_str("    ");
                out.push_str(&format_kb_desk_row(d));
                out.push('\n');
            }

            let comment_items = body["kb"]["comments"]["items"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let total_open = body["kb"]["comments"]["total_open"].as_u64().unwrap_or(0);
            let comments_truncated = body["kb"]["comments"]["truncated"]
                .as_bool()
                .unwrap_or(false);
            out.push_str(&format!(
                "  comments: total_open={total_open} items={}{}\n",
                comment_items.len(),
                if comments_truncated {
                    " (truncated)"
                } else {
                    ""
                },
            ));
            for c in &comment_items {
                out.push_str("    ");
                out.push_str(&format_kb_comment_row(c));
                out.push('\n');
            }
        }
        Some(false) => {
            let reason = kb_reason(body).unwrap_or_else(|| "unknown".to_string());
            out.push_str(&format!("  unavailable ({reason})\n"));
        }
        None => out.push_str("  (no kb lane in response)\n"),
    }

    out
}

// --- HTTP shell ------------------------------------------------------------

fn http_client() -> Result<reqwest::Client> {
    // V70-A2 — the shared builder (default `X-Kbc-Request: 1`).
    crate::client_builder()
        .timeout(Duration::from_secs(10))
        .build()
        .context("build http client")
}

async fn fetch_inbox(client: &reqwest::Client, daemon: &str) -> Result<Value> {
    let url = format!("{}/api/inbox", daemon.trim_end_matches('/'));
    let body: Value = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url} — is kb-code-server running at {daemon}?"))?
        .error_for_status()
        .with_context(|| format!("GET {url}"))?
        .json()
        .await
        .with_context(|| format!("parse {url} response as JSON"))?;
    if body["schema"].as_str() != Some(SCHEMA) {
        eprintln!(
            "[kb-code inbox] warning: unexpected schema {:?} (expected {SCHEMA:?})",
            body["schema"].as_str().unwrap_or("?"),
        );
    }
    Ok(body)
}

/// `kb-code inbox` (one-shot): `GET /api/inbox`, render the three lanes
/// human-readably, or `--json` to print the raw body verbatim.
pub async fn run_once(daemon: &str, json: bool) -> Result<()> {
    let client = http_client()?;
    let body = fetch_inbox(&client, daemon).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
    } else {
        print!("{}", format_body_human(&body));
    }
    Ok(())
}

/// `kb-code inbox --watch [--interval SECS]` — a plain HTTP poll loop (see
/// the module doc for why this isn't SSE-driven like `annotate watch`).
/// Prints the current snapshot once (doubling as the seed pass), then
/// polls every `interval_secs`, printing only new/changed rows and kb-lane
/// availability flips (`available` true↔false, printed once per flip —
/// never once per poll). Clean exit on Ctrl-C / SIGTERM.
pub async fn run_watch(daemon: &str, json: bool, interval_secs: u64) -> Result<()> {
    let client = http_client()?;
    let body = fetch_inbox(&client, daemon).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
    } else {
        print!("{}", format_body_human(&body));
    }

    let mut seen: HashSet<InboxSeenKey> = HashSet::new();
    seed_seen(&mut seen, &rows_from_body(&body));
    let mut last_available = kb_available(&body);

    eprintln!("[kb-code inbox] watching {daemon} (poll every {interval_secs}s) — Ctrl-C to stop");

    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    #[cfg(unix)]
    let mut sigterm =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();

    loop {
        tokio::select! {
            biased;
            _ = &mut ctrl_c => {
                eprintln!("[kb-code inbox] received SIGINT; exiting");
                return Ok(());
            }
            _ = async {
                #[cfg(unix)]
                { if let Some(s) = sigterm.as_mut() { s.recv().await; } else { std::future::pending::<()>().await; } }
                #[cfg(not(unix))]
                { std::future::pending::<()>().await; }
            } => {
                eprintln!("[kb-code inbox] received SIGTERM; exiting");
                return Ok(());
            }
            _ = tokio::time::sleep(Duration::from_secs(interval_secs)) => {}
        }

        let body = match fetch_inbox(&client, daemon).await {
            Ok(b) => b,
            Err(e) => {
                eprintln!("[kb-code inbox] poll failed: {e}; retrying in {interval_secs}s …");
                continue;
            }
        };

        let available = kb_available(&body);
        if available != last_available {
            match available {
                Some(true) => eprintln!("[kb-code inbox] kb lane: available"),
                Some(false) => {
                    let reason = kb_reason(&body).unwrap_or_else(|| "unknown".to_string());
                    eprintln!("[kb-code inbox] kb lane: unavailable ({reason})");
                }
                None => eprintln!("[kb-code inbox] kb lane: absent from response"),
            }
            last_available = available;
        }

        let rows = rows_from_body(&body);
        for row in &take_new(&mut seen, &rows) {
            if json {
                println!("{}", row.value);
            } else {
                println!("{}", format_row(row));
            }
        }
    }
}

/// Splice `"lane": tag` into a row's own `value` object — never a nested
/// envelope, same rule `watch::apply_lane_tag` documents (duplicated here
/// rather than shared: these two modules keep no shared private surface,
/// matching this crate's existing "small enough to copy" convention for
/// hook adapters).
fn apply_lane_tag(mut v: Value, tag: Option<&str>) -> Value {
    if let Some(tag) = tag {
        if let Some(obj) = v.as_object_mut() {
            obj.insert("lane".to_string(), Value::String(tag.to_string()));
        }
    }
    v
}

fn emit_row(row: &InboxRow, json: bool, lane_tag: Option<&str>) {
    if json {
        println!("{}", apply_lane_tag(row.value.clone(), lane_tag));
    } else {
        match lane_tag {
            Some(tag) => println!("[{tag}] {}", format_row(row)),
            None => println!("{}", format_row(row)),
        }
    }
}

fn reset_idle(deadline: &mut Option<Instant>, timeout_secs: Option<u64>) {
    if let Some(s) = timeout_secs {
        *deadline = Some(Instant::now() + Duration::from_secs(s));
    }
}

async fn sleep_until_opt(deadline: Option<Instant>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(d).await,
        None => std::future::pending::<()>().await,
    }
}

/// `kb-code watch inbox …` (V71-X1) — the inbox lane of the unified
/// `kb-code watch` command. SAME poll shell as [`run_watch`] (same
/// endpoint, same `kb`-lane-availability eprintln, same Ctrl-C/SIGTERM
/// handling), but SILENT on the initial fetch — seed-then-diff via
/// [`seed_seen_since`], matching `annotate watch`'s own default — rather
/// than dumping the whole snapshot as pretty JSON: a `kb-code watch`
/// invocation runs several lanes CONCURRENTLY in one process
/// (`main.rs::watch_unified_cmd`), and only a one-line-per-row shape is
/// safe to interleave with another lane's own output. Every extraction/
/// dedupe/render function this calls (`rows_from_body`, `take_new`,
/// `format_row`, `kb_available`/`kb_reason`) is the SAME one [`run_watch`]
/// uses — this is a different SHELL around identical pure logic, not a
/// second implementation of it. `--timeout` is an IDLE timeout exactly
/// like `annotate watch`'s own (resets on every surfaced row; fires when
/// this lane alone has gone quiet that long — independent of the other
/// concurrently-running lane's own activity).
#[allow(clippy::too_many_arguments)]
pub async fn run_watch_lane(
    daemon: &str,
    json: bool,
    interval_secs: u64,
    backlog: bool,
    since: Option<i64>,
    once: bool,
    timeout_secs: Option<u64>,
    lane_tag: Option<&str>,
) -> Result<()> {
    let client = http_client()?;
    let body = fetch_inbox(&client, daemon).await?;
    let mut seen: HashSet<InboxSeenKey> = HashSet::new();
    let mut last_available = kb_available(&body);
    let surfaced = seed_seen_since(&mut seen, &rows_from_body(&body), backlog, since);
    let mut deadline: Option<Instant> =
        timeout_secs.map(|s| Instant::now() + Duration::from_secs(s));
    for row in &surfaced {
        emit_row(row, json, lane_tag);
        reset_idle(&mut deadline, timeout_secs);
    }
    if once && !surfaced.is_empty() {
        return Ok(());
    }

    let tag_prefix = lane_tag
        .map(|t| format!("watch/{t}"))
        .unwrap_or_else(|| "inbox".to_string());
    eprintln!(
        "[kb-code {tag_prefix}] watching {daemon} (poll every {interval_secs}s) — Ctrl-C to stop"
    );

    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    #[cfg(unix)]
    let mut sigterm =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();

    loop {
        tokio::select! {
            biased;
            _ = &mut ctrl_c => {
                eprintln!("[kb-code {tag_prefix}] received SIGINT; exiting");
                return Ok(());
            }
            _ = async {
                #[cfg(unix)]
                { if let Some(s) = sigterm.as_mut() { s.recv().await; } else { std::future::pending::<()>().await; } }
                #[cfg(not(unix))]
                { std::future::pending::<()>().await; }
            } => {
                eprintln!("[kb-code {tag_prefix}] received SIGTERM; exiting");
                return Ok(());
            }
            _ = sleep_until_opt(deadline) => {
                return Ok(());
            }
            _ = tokio::time::sleep(Duration::from_secs(interval_secs)) => {}
        }

        let body = match fetch_inbox(&client, daemon).await {
            Ok(b) => b,
            Err(e) => {
                eprintln!(
                    "[kb-code {tag_prefix}] poll failed: {e}; retrying in {interval_secs}s …"
                );
                continue;
            }
        };

        let available = kb_available(&body);
        if available != last_available {
            match available {
                Some(true) => eprintln!("[kb-code {tag_prefix}] kb lane: available"),
                Some(false) => {
                    let reason = kb_reason(&body).unwrap_or_else(|| "unknown".to_string());
                    eprintln!("[kb-code {tag_prefix}] kb lane: unavailable ({reason})");
                }
                None => eprintln!("[kb-code {tag_prefix}] kb lane: absent from response"),
            }
            last_available = available;
        }

        let rows = rows_from_body(&body);
        let fresh = take_new(&mut seen, &rows);
        for row in &fresh {
            emit_row(row, json, lane_tag);
            reset_idle(&mut deadline, timeout_secs);
        }
        if once && !fresh.is_empty() {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // --- fixtures, per the pinned `unified-inbox/1` schema ------------------

    fn review_row_json(id: i64, updated_at: i64, unanswered: i64, unresolved: i64) -> Value {
        json!({
            "review_id": id,
            "repo": "kb",
            "pr_number": 7,
            "title": "widget refactor",
            "unresolved_findings": unresolved,
            "unanswered_questions": unanswered,
            "verdict": {"state": "pending"},
            "verdict_stale": false,
            "pr_head_drift": false,
            "updated_at": updated_at,
        })
    }

    fn annotation_row_json(id: &str, updated_at: i64) -> Value {
        json!({
            "id": id,
            "repo": "kb",
            "path": "src/lib.rs",
            "line": 42,
            "intent": "question",
            "author": "you",
            "excerpt": "why is this guarded?",
            "reply_count": 1,
            "updated_at": updated_at,
        })
    }

    fn desk_item_json(kb: &str, id: &str, updated_unix: i64) -> Value {
        json!({
            "kb": kb,
            "id": id,
            "source_relative": "handoff/note.html",
            "title": "handoff note",
            "updated_unix": updated_unix,
            "expires_at": null,
            "session_id": null,
            "comments_open": 1,
            "comments_total": 2,
            "read_state": "unread",
            "last_opened_unix": null,
            "changed_since_read": false,
        })
    }

    fn comment_item_json(kb: &str, comment_id: &str, updated_at: i64) -> Value {
        json!({
            "kb": kb,
            "artifact_id": "abc123",
            "source_relative": "notes/foo.html",
            "title": "foo notes",
            "comment_id": comment_id,
            "excerpt": "left a question here",
            "author": "you",
            "reply_count": 0,
            "anchor": "section",
            "stale": false,
            "created_at": 100,
            "updated_at": updated_at,
        })
    }

    fn full_body_available() -> Value {
        json!({
            "schema": "unified-inbox/1",
            "reviews": [review_row_json(1, 1000, 2, 1)],
            "annotations": [annotation_row_json("a1", 2000)],
            "kb": {
                "available": true,
                "reason": null,
                "desk": {
                    "items": [desk_item_json("memory", "d1", 3000)],
                    "attention": 3,
                },
                "comments": {
                    "items": [comment_item_json("memory", "c1", 4000)],
                    "total_open": 7,
                },
            },
        })
    }

    fn degraded_body(reason: &str) -> Value {
        json!({
            "schema": "unified-inbox/1",
            "reviews": [],
            "annotations": [],
            "kb": {
                "available": false,
                "reason": reason,
                "desk": null,
                "comments": null,
            },
        })
    }

    // --- seen-key extraction --------------------------------------------

    #[test]
    fn review_row_key_uses_computed_score() {
        // score = unanswered*2 + unresolved = 2*2 + 1 = 5
        let row = review_row_json(1, 1000, 2, 1);
        let extracted = review_row(&row).unwrap();
        assert_eq!(
            extracted.key,
            InboxSeenKey::Review {
                review_id: 1,
                updated_at: 1000,
                score: 5,
            }
        );
        assert_eq!(extracted.lane, Lane::Review);
    }

    #[test]
    fn review_row_missing_review_id_is_skipped() {
        let row = json!({"repo": "kb"});
        assert!(review_row(&row).is_none());
    }

    #[test]
    fn annotation_row_key_is_id_and_updated_at() {
        let row = annotation_row_json("a1", 2000);
        let extracted = annotation_row(&row).unwrap();
        assert_eq!(
            extracted.key,
            InboxSeenKey::Annotation {
                id: "a1".to_string(),
                updated_at: 2000,
            }
        );
        assert_eq!(extracted.lane, Lane::Annotation);
    }

    #[test]
    fn kb_desk_row_key_is_kb_id_and_updated_unix() {
        let row = desk_item_json("memory", "d1", 3000);
        let extracted = kb_desk_row(&row).unwrap();
        assert_eq!(
            extracted.key,
            InboxSeenKey::KbDesk {
                kb: "memory".to_string(),
                id: "d1".to_string(),
                updated_unix: 3000,
            }
        );
        assert_eq!(extracted.lane, Lane::KbDesk);
    }

    #[test]
    fn kb_comment_row_key_is_kb_comment_id_and_updated_at() {
        let row = comment_item_json("memory", "c1", 4000);
        let extracted = kb_comment_row(&row).unwrap();
        assert_eq!(
            extracted.key,
            InboxSeenKey::KbComment {
                kb: "memory".to_string(),
                comment_id: "c1".to_string(),
                updated_at: 4000,
            }
        );
        assert_eq!(extracted.lane, Lane::KbComment);
    }

    #[test]
    fn rows_from_body_flattens_all_four_lanes_in_order() {
        let body = full_body_available();
        let rows = rows_from_body(&body);
        assert_eq!(rows.len(), 4);
        assert_eq!(
            rows.iter().map(|r| r.lane).collect::<Vec<_>>(),
            vec![
                Lane::Review,
                Lane::Annotation,
                Lane::KbDesk,
                Lane::KbComment
            ]
        );
    }

    #[test]
    fn rows_from_body_omits_kb_lanes_when_unavailable() {
        for reason in ["disabled", "unreachable", "sibling_mismatch"] {
            let body = degraded_body(reason);
            let rows = rows_from_body(&body);
            assert!(
                rows.is_empty(),
                "expected no rows for reason {reason:?}, got {rows:?}"
            );
        }
    }

    #[test]
    fn kb_available_and_reason_read_the_pinned_pointer() {
        let body = full_body_available();
        assert_eq!(kb_available(&body), Some(true));
        assert_eq!(kb_reason(&body), None);

        for reason in ["disabled", "unreachable", "sibling_mismatch"] {
            let body = degraded_body(reason);
            assert_eq!(kb_available(&body), Some(false));
            assert_eq!(kb_reason(&body).as_deref(), Some(reason));
        }
    }

    #[test]
    fn kb_available_is_none_when_kb_lane_absent() {
        let body = json!({"schema": "unified-inbox/1", "reviews": [], "annotations": []});
        assert_eq!(kb_available(&body), None);
    }

    // --- diff logic ----------------------------------------------------

    #[test]
    fn take_new_returns_rows_only_once() {
        let mut seen = HashSet::new();
        let rows = rows_from_body(&full_body_available());
        let first = take_new(&mut seen, &rows);
        assert_eq!(first.len(), 4);
        let second = take_new(&mut seen, &rows);
        assert!(second.is_empty(), "unchanged rows must not re-surface");
    }

    #[test]
    fn take_new_treats_an_updated_at_change_as_new() {
        let mut seen = HashSet::new();
        let body_v1 = full_body_available();
        seed_seen(&mut seen, &rows_from_body(&body_v1));

        // Same annotation id, but its updated_at moved (e.g. a reply
        // landed) — must surface again.
        let mut body_v2 = full_body_available();
        body_v2["annotations"][0]["updated_at"] = json!(2500);
        let changed = take_new(&mut seen, &rows_from_body(&body_v2));
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].lane, Lane::Annotation);
    }

    #[test]
    fn take_new_treats_a_score_change_as_new_even_with_same_updated_at() {
        // A review row whose score changed but whose updated_at (by
        // fixture construction) didn't — still a distinct key, since score
        // rides the tuple too.
        let mut seen = HashSet::new();
        let row_v1 = review_row_json(1, 1000, 2, 1); // score 5
        seed_seen(&mut seen, &[review_row(&row_v1).unwrap()]);

        let row_v2 = review_row_json(1, 1000, 3, 1); // score 7
        let changed = take_new(&mut seen, &[review_row(&row_v2).unwrap()]);
        assert_eq!(changed.len(), 1);
    }

    #[test]
    fn seed_seen_marks_everything_without_returning_anything() {
        let mut seen = HashSet::new();
        let rows = rows_from_body(&full_body_available());
        seed_seen(&mut seen, &rows);
        assert_eq!(seen.len(), 4);
        // A subsequent take_new over the SAME rows now sees nothing new.
        assert!(take_new(&mut seen, &rows).is_empty());
    }

    // V71-X1 — `seed_seen_since`, `kb-code watch`'s inbox-lane seeding.
    #[test]
    fn seed_seen_since_surfaces_only_rows_at_or_after_the_window() {
        // review=1000, annotation=2000, kb-desk=3000, kb-comment=4000.
        let mut seen = HashSet::new();
        let rows = rows_from_body(&full_body_available());
        let surfaced = seed_seen_since(&mut seen, &rows, false, Some(3000));
        assert_eq!(
            surfaced.len(),
            2,
            "only kb-desk (3000) and kb-comment (4000) qualify"
        );
        assert!(surfaced.iter().all(|r| r.key.timestamp() >= 3000));
        // Every row is still seeded regardless — a later identical refetch
        // sees nothing new.
        assert_eq!(seen.len(), 4);
        assert!(take_new(&mut seen, &rows).is_empty());
    }

    #[test]
    fn seed_seen_since_none_never_surfaces_without_backlog() {
        let mut seen = HashSet::new();
        let rows = rows_from_body(&full_body_available());
        assert!(seed_seen_since(&mut seen, &rows, false, None).is_empty());
    }

    #[test]
    fn seed_seen_since_backlog_surfaces_everything_regardless_of_since() {
        let mut seen = HashSet::new();
        let rows = rows_from_body(&full_body_available());
        let surfaced = seed_seen_since(&mut seen, &rows, true, Some(999_999));
        assert_eq!(surfaced.len(), 4);
    }

    #[test]
    fn apply_lane_tag_splices_flat_and_is_a_no_op_for_none() {
        let row = review_row(&review_row_json(1, 1000, 2, 1)).unwrap();
        let tagged = apply_lane_tag(row.value.clone(), Some("inbox"));
        assert_eq!(tagged["lane"], "inbox");
        assert_eq!(tagged["review_id"], row.value["review_id"]);
        assert_eq!(apply_lane_tag(row.value.clone(), None), row.value);
    }

    // --- render helpers --------------------------------------------------

    #[test]
    fn format_review_row_matches_review_inbox_grammar() {
        let row = review_row_json(1, 1000, 2, 1);
        let line = format_review_row(&row);
        assert!(line.starts_with("review 1"));
        assert!(line.contains("kb"));
        assert!(line.contains("#7"));
        assert!(line.contains("unanswered=2"));
        assert!(line.contains("unresolved=1"));
        assert!(line.contains("widget refactor"));
    }

    #[test]
    fn format_review_row_renders_dash_with_no_pr() {
        let mut row = review_row_json(1, 1000, 0, 0);
        row["pr_number"] = Value::Null;
        let line = format_review_row(&row);
        assert!(
            line.split_whitespace().any(|tok| tok == "-"),
            "line was: {line:?}"
        );
    }

    #[test]
    fn format_annotation_row_renders_path_line_and_excerpt() {
        let row = annotation_row_json("a1", 2000);
        let line = format_annotation_row(&row);
        assert!(line.contains("a1"));
        assert!(line.contains("src/lib.rs:42"));
        assert!(line.contains("question"));
        assert!(line.contains("replies=1"));
        assert!(line.contains("why is this guarded?"));
    }

    #[test]
    fn format_annotation_row_falls_back_to_bare_path_without_a_line() {
        let mut row = annotation_row_json("a1", 2000);
        row["line"] = Value::Null;
        let line = format_annotation_row(&row);
        assert!(line.contains("src/lib.rs"));
        assert!(!line.contains("src/lib.rs:"));
    }

    #[test]
    fn format_kb_desk_row_renders_kb_id_and_read_state() {
        let row = desk_item_json("memory", "d1", 3000);
        let line = format_kb_desk_row(&row);
        assert!(line.contains("memory"));
        assert!(line.contains("d1"));
        assert!(line.contains("read=unread"));
        assert!(line.contains("changed=false"));
        assert!(line.contains("handoff note"));
    }

    #[test]
    fn format_kb_comment_row_renders_kb_comment_and_author() {
        let row = comment_item_json("memory", "c1", 4000);
        let line = format_kb_comment_row(&row);
        assert!(line.contains("memory"));
        assert!(line.contains("c1"));
        assert!(line.contains("by you"));
        assert!(line.contains("replies=0"));
        assert!(line.contains("left a question here"));
    }

    #[test]
    fn format_row_dispatches_by_lane() {
        let rows = rows_from_body(&full_body_available());
        let rendered: Vec<String> = rows.iter().map(format_row).collect();
        assert_eq!(rendered.len(), 4);
        assert!(rendered[0].starts_with("review"));
        assert!(rendered[1].starts_with("annotation"));
        assert!(rendered[2].starts_with("desk"));
        assert!(rendered[3].starts_with("comment"));
    }

    #[test]
    fn format_body_human_includes_section_headers_and_counts() {
        let out = format_body_human(&full_body_available());
        assert!(out.contains("Reviews awaiting you (1)"));
        assert!(out.contains("Open questions in working trees (1)"));
        assert!(out.contains("From kb"));
        assert!(out.contains("attention=3"));
        assert!(out.contains("total_open=7"));
    }

    #[test]
    fn format_body_human_prints_none_placeholders_when_lanes_empty() {
        let body = json!({
            "schema": "unified-inbox/1",
            "reviews": [],
            "annotations": [],
            "kb": {"available": true, "reason": null,
                   "desk": {"items": [], "attention": 0},
                   "comments": {"items": [], "total_open": 0}},
        });
        let out = format_body_human(&body);
        assert_eq!(out.matches("(none)").count(), 2);
    }

    #[test]
    fn format_body_human_reports_each_degraded_reason_honestly() {
        for reason in ["disabled", "unreachable", "sibling_mismatch"] {
            let out = format_body_human(&degraded_body(reason));
            assert!(
                out.contains(&format!("unavailable ({reason})")),
                "expected reason {reason:?} in: {out}"
            );
            // Never an empty section pretending zero — no "attention=" or
            // "total_open=" line should render for a degraded lane.
            assert!(!out.contains("attention="));
            assert!(!out.contains("total_open="));
        }
    }

    #[test]
    fn format_body_human_shows_truncated_flag_when_cut() {
        let mut body = full_body_available();
        body["kb"]["desk"]["truncated"] = json!(true);
        body["kb"]["comments"]["truncated"] = json!(true);
        let out = format_body_human(&body);
        assert_eq!(out.matches("(truncated)").count(), 2);
    }

    #[test]
    fn format_body_human_omits_truncated_tag_when_not_cut() {
        let out = format_body_human(&full_body_available());
        assert!(!out.contains("(truncated)"));
    }
}
