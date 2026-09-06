//! `kb context <query>` — CT-D1: the ONE deterministic, budgeted context
//! pack, rendered.
//!
//! ZERO composition happens here. `GET /api/context` (`kb-server`'s
//! `routes::context`) already joined recall, recollect POINTERS, the open
//! comments on matching artifacts, and the kb-local `code_refs` summary into
//! one budgeted pack; this verb fetches it and renders it. `--json` is the
//! machine form, printed verbatim — the human rendering below adds no fact
//! the JSON doesn't carry, so an agent and a person read the same pack.
//!
//! **Truncation is never silent, in either shape.** Every lane whose
//! `<lane>_truncated` flag is set gets an explicit "…N more, not shown"
//! line, and a pack whose CHAR budget bit says so plus how to raise it.
//! That mirrors the route's own contract (the CT-E1 daycard lane-cap
//! precedent) — a pack that quietly dropped half its evidence would be worse
//! than no pack.
//!
//! **Scent, not substance.** The sessions lane is POINTERS (id + name + a
//! one-line digest excerpt) — invariant #11's R0/R1/R3. To read one, run
//! `kb sessions read <id>`; this verb will never print a transcript.

use crate::http;
use anyhow::{anyhow, Result};

/// Printed verbatim as the human footer AND the `--json` `note` field, so
/// both shapes carry the same caveat about what the pack is.
const PACK_NOTE: &str =
    "the pack POINTS: sessions are ids + digest excerpts (kb sessions read <id>), \
code paths are kb-local citation hints, never a claim the code still matches";

/// `kb context <query> [--cwd] [--budget] [--session] [--json]` entry point.
#[allow(clippy::too_many_arguments)]
pub async fn context(
    query: &str,
    cwd: Option<&str>,
    budget: Option<u32>,
    session: Option<&str>,
    no_floor: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
    json: bool,
) -> Result<()> {
    match fetch(query, cwd, budget, session, no_floor, daemon, bearer).await {
        Ok(pack) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&pack)?);
            } else {
                println!("{}", render_human(&pack));
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

#[allow(clippy::too_many_arguments)]
async fn fetch(
    query: &str,
    cwd: Option<&str>,
    budget: Option<u32>,
    session: Option<&str>,
    no_floor: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<serde_json::Value> {
    let url = http::detect_daemon(daemon, bearer).await.ok_or_else(|| {
        anyhow!(
            "daemon not reachable{} — start it with `kb daemon`",
            daemon.map(|d| format!(" at {d}")).unwrap_or_default()
        )
    })?;
    let client = http::client_with_timeout_and_bearer(30, bearer)?;
    let mut req = client
        .get(format!("{url}/api/context"))
        .query(&[("q", query)]);
    if let Some(c) = cwd {
        req = req.query(&[("cwd", c)]);
    }
    if let Some(b) = budget {
        req = req.query(&[("budget", b.to_string())]);
    }
    if let Some(s) = session {
        req = req.query(&[("session", s)]);
    }
    if no_floor {
        req = req.query(&[("no_floor", "true")]);
    }
    // B1 — mirror `kb recall --scope auto`'s project narrowing on the
    // client side: derive the same repo slug (from `--cwd` if given, else
    // the process cwd) and pass it as `memory_project`/`memory_visible_to`
    // so a daemon that understands them can narrow the pack's memories lane
    // the same way. Additive/best-effort: an older daemon's serde `Query`
    // ignores unrecognised params, so this degrades gracefully rather than
    // erroring — see `commands::memory::resolve_recall_wire`'s doc comment
    // for the mirror-image wiring on the recall side.
    let slug = match cwd {
        Some(c) => crate::commands::memory::current_repo_slug_in(&std::path::PathBuf::from(c)),
        None => crate::commands::memory::current_repo_slug(),
    };
    if !slug.is_empty() {
        let project = format!("memory-{slug}");
        let visible_to = format!("{slug},{project}");
        req = req
            .query(&[("memory_project", project.as_str())])
            .query(&[("memory_visible_to", visible_to.as_str())]);
    }
    let mut pack = http::send_json(req, "context").await?;
    // The route is deliberately free of prose; the CLI owns the caveat, and
    // owns it in BOTH shapes (see `PACK_NOTE`).
    if let Some(obj) = pack.as_object_mut() {
        obj.insert("note".into(), serde_json::Value::String(PACK_NOTE.into()));
    }
    Ok(pack)
}

/// Pure text renderer over the SAME shape `--json` prints — split out so the
/// layout is unit-testable with a literal fixture, no daemon required.
fn render_human(pack: &serde_json::Value) -> String {
    let mut s = String::new();
    let q = pack["q"].as_str().unwrap_or("");
    s.push_str(&format!("context for: {q}\n"));
    if let Some(cwd) = pack["cwd"].as_str() {
        s.push_str(&format!("cwd: {cwd}\n"));
    }
    s.push_str(&format!(
        "{}\n",
        pack["scent"].as_str().unwrap_or("no prior context")
    ));
    s.push('\n');

    render_memories(pack, &mut s);
    render_sessions(pack, &mut s);
    render_comments(pack, &mut s);
    render_code_hints(pack, &mut s);

    // The budget line is only printed when it actually BIT — an unremarkable
    // pack shouldn't spend a line telling you nothing happened.
    if pack["budget_exceeded"].as_bool().unwrap_or(false) {
        let budget = pack["budget"].as_u64().unwrap_or(0);
        s.push_str(&format!(
            "budget: {budget} chars exhausted — items above were dropped to fit; re-run with --budget <bigger>\n"
        ));
    }
    s.push_str(pack["note"].as_str().unwrap_or(PACK_NOTE));
    s.push('\n');
    s
}

/// "…N more, not shown" — the ONE place a lane's truncation is worded, so
/// four lanes can't drift into four phrasings. `total` is the pre-cap count
/// the route reports; `shown` is what survived.
fn more_line(total: u64, shown: usize) -> Option<String> {
    let hidden = total.saturating_sub(shown as u64);
    if hidden == 0 {
        return None;
    }
    Some(format!("  …{hidden} more, not shown\n"))
}

fn lane<'a>(pack: &'a serde_json::Value, key: &str) -> Vec<&'a serde_json::Value> {
    pack[key]
        .as_array()
        .map(|a| a.iter().collect())
        .unwrap_or_default()
}

fn render_memories(pack: &serde_json::Value, s: &mut String) {
    let rows = lane(pack, "memories");
    let total = pack["memories_total"].as_u64().unwrap_or(0);
    if rows.is_empty() && total == 0 {
        return;
    }
    s.push_str("memories\n");
    for m in &rows {
        // CT-C1/CT-C3 prefix composition, same order the kb-recall hook
        // uses: disputed first, then the failed-outcome warning.
        let mut prefix = String::new();
        if m["flagged"].as_bool().unwrap_or(false) {
            prefix.push_str("⚠ disputed: ");
        }
        if m["warns"].as_bool().unwrap_or(false) {
            prefix.push_str("✗ didn't work: ");
        }
        let title = m["title"].as_str().unwrap_or("?");
        let kb = m["kb"].as_str().unwrap_or("?");
        let id = m["id"].as_str().unwrap_or("?");
        let drift = m["drift_open"].as_u64().unwrap_or(0);
        let drift_suffix = if drift > 0 {
            format!(" [⚠ {drift} drift-flagged citation(s)]")
        } else {
            String::new()
        };
        s.push_str(&format!(
            "  {prefix}{title}  [{kb}]  (id {id}){drift_suffix}\n"
        ));
        if let Some(sum) = m["summary"].as_str().filter(|t| !t.is_empty()) {
            s.push_str(&format!("    ↳ {sum}\n"));
        }
        // Invariant #10's decomposition — the reason a hit is trustworthy.
        if let Some(score) = m["score"].as_f64() {
            let sal = m["salience"].as_f64().unwrap_or(0.0);
            let mut bits = format!("score {score:.4} = salience {sal:.2}");
            if let Some(rel) = m["rel"].as_f64() {
                bits.push_str(&format!(" × rel {rel:.4}"));
            }
            if let Some(decay) = m["decay"].as_f64() {
                bits.push_str(&format!(" × decay {decay:.4}"));
            }
            s.push_str(&format!("    {bits}\n"));
        }
    }
    if let Some(l) = more_line(total, rows.len()) {
        s.push_str(&l);
    }
    s.push('\n');
}

fn render_sessions(pack: &serde_json::Value, s: &mut String) {
    let rows = lane(pack, "sessions");
    let total = pack["sessions_total"].as_u64().unwrap_or(0);
    if rows.is_empty() && total == 0 {
        return;
    }
    s.push_str("prior sessions (pointers — kb sessions read <id>)\n");
    for r in &rows {
        let name = r["display_name"].as_str().unwrap_or("?");
        let sid = r["session_id"].as_str().unwrap_or("?");
        let here = if r["same_cwd"].as_bool().unwrap_or(false) {
            " · here"
        } else {
            ""
        };
        s.push_str(&format!("  {name}  ({sid}){here}\n"));
        // R3: recency/staleness, errors and commits are SURFACED signals the
        // agent weighs — never filters, never score terms.
        let age = r["age_days"].as_i64().unwrap_or(0);
        let commits = r["commit_count"].as_u64().unwrap_or(0);
        let errors = r["error_count"].as_u64().unwrap_or(0);
        let stale = if r["stale"].as_bool().unwrap_or(false) {
            " · STALE"
        } else {
            ""
        };
        s.push_str(&format!(
            "    {age}d ago · {commits} commit(s) · {errors} error(s){stale}\n"
        ));
        if let Some(x) = r["excerpt"].as_str().filter(|t| !t.is_empty()) {
            s.push_str(&format!("    ↳ {x}\n"));
        }
    }
    if let Some(l) = more_line(total, rows.len()) {
        s.push_str(&l);
    }
    s.push('\n');
}

fn render_comments(pack: &serde_json::Value, s: &mut String) {
    let rows = lane(pack, "comments");
    let total = pack["comments_total"].as_u64().unwrap_or(0);
    if rows.is_empty() && total == 0 {
        return;
    }
    s.push_str("open comments on matching artifacts\n");
    for r in &rows {
        let title = r["title"].as_str().unwrap_or("?");
        let kb = r["kb"].as_str().unwrap_or("?");
        let cid = r["comment_id"].as_str().unwrap_or("?");
        let author = r["author"].as_str().unwrap_or("?");
        let replies = r["reply_count"].as_u64().unwrap_or(0);
        let stale = if r["stale"].as_bool().unwrap_or(false) {
            " · anchor stale"
        } else {
            ""
        };
        s.push_str(&format!(
            "  {title}  [{kb}]  ({cid}) · by {author} · {replies} repl(y/ies){stale}\n"
        ));
        if let Some(x) = r["excerpt"].as_str().filter(|t| !t.is_empty()) {
            s.push_str(&format!("    ↳ {x}\n"));
        }
    }
    if let Some(l) = more_line(total, rows.len()) {
        s.push_str(&l);
    }
    s.push('\n');
}

fn render_code_hints(pack: &serde_json::Value, s: &mut String) {
    let rows = lane(pack, "code_hints");
    let total = pack["code_hints_total"].as_u64().unwrap_or(0);
    if rows.is_empty() && total == 0 {
        return;
    }
    s.push_str("code paths these artifacts cite (kb-local hints)\n");
    for r in &rows {
        let path = r["path_hint"].as_str().unwrap_or("?");
        let n = r["cited_by"].as_u64().unwrap_or(0);
        s.push_str(&format!("  {path}  (cited by {n})\n"));
    }
    if let Some(l) = more_line(total, rows.len()) {
        s.push_str(&l);
    }
    s.push('\n');
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
    use serde_json::json;

    fn empty_pack() -> serde_json::Value {
        json!({
            "q": "wire the thing",
            "budget": 4000,
            "scent": "no prior context",
            "memories": [], "memories_total": 0, "memories_truncated": false,
            "sessions": [], "sessions_total": 0, "sessions_truncated": false,
            "comments": [], "comments_total": 0, "comments_truncated": false,
            "code_hints": [], "code_hints_total": 0, "code_hints_truncated": false,
            "artifacts_matched": 0,
            "budget_exceeded": false,
            "chars": 0,
            "ms": 3,
            "note": PACK_NOTE,
        })
    }

    #[test]
    fn an_empty_pack_renders_honest_zeros_and_no_empty_headings() {
        let out = render_human(&empty_pack());
        assert!(out.contains("context for: wire the thing"));
        assert!(out.contains("no prior context"));
        // No lane HEADING may appear for a lane with nothing in it. (Matched
        // on the exact heading text, not a substring of it: `PACK_NOTE`
        // legitimately mentions "code paths" in its caveat, and a loose
        // assertion here would fail for the wrong reason.)
        assert!(!out.contains("memories\n"));
        assert!(!out.contains("prior sessions (pointers"));
        assert!(!out.contains("open comments on matching artifacts"));
        assert!(!out.contains("code paths these artifacts cite"));
        assert!(out.contains(PACK_NOTE));
    }

    #[test]
    fn a_full_pack_renders_every_lane_with_its_decomposition() {
        let mut p = empty_pack();
        p["scent"] = json!("1 prior session · 1 open comment · 1 memory");
        p["memories"] = json!([{
            "id": "abc123def456", "kb": "memory", "title": "BFQ made ionice real",
            "source_relative": "m/abc.html", "summary": "mq-deadline ignored ionice",
            "score": 0.1234, "salience": 0.6, "rank": 0, "rel": 0.0166, "decay": 0.87,
        }]);
        p["memories_total"] = json!(1);
        p["sessions"] = json!([{
            "session_id": "s-1", "kb": "sessions", "display_name": "wire the thing",
            "started_at": 100, "age_days": 4, "stale": false,
            "commit_count": 2, "error_count": 0, "excerpt": "landed the route",
            "same_cwd": true,
        }]);
        p["sessions_total"] = json!(1);
        p["comments"] = json!([{
            "kb": "docs", "artifact_id": "aaa111bbb222", "title": "the design",
            "comment_id": "c-1", "excerpt": "is this still true?", "author": "you",
            "reply_count": 0, "stale": false, "updated_at": 200,
        }]);
        p["comments_total"] = json!(1);
        p["code_hints"] = json!([{ "path_hint": "src/routes/context.rs", "cited_by": 2 }]);
        p["code_hints_total"] = json!(1);

        let out = render_human(&p);
        assert!(out.contains("BFQ made ionice real"));
        assert!(out.contains("↳ mq-deadline ignored ionice"));
        // invariant #10's decomposition is rendered, not just the score.
        assert!(out.contains("score 0.1234 = salience 0.60 × rel 0.0166 × decay 0.8700"));
        assert!(out.contains("wire the thing  (s-1) · here"));
        assert!(out.contains("4d ago · 2 commit(s) · 0 error(s)"));
        assert!(out.contains("the design  [docs]  (c-1) · by you"));
        assert!(out.contains("src/routes/context.rs  (cited by 2)"));
        // A pack that fit its budget must not print a budget line.
        assert!(!out.contains("budget:"));
    }

    #[test]
    fn truncated_lanes_say_how_many_are_hidden() {
        let mut p = empty_pack();
        p["memories"] =
            json!([{ "id": "a", "kb": "m", "title": "one", "score": 0.1, "salience": 0.5 }]);
        p["memories_total"] = json!(7);
        p["memories_truncated"] = json!(true);
        let out = render_human(&p);
        assert!(
            out.contains("…6 more, not shown"),
            "truncation must never be silent: {out}"
        );
    }

    #[test]
    fn an_exhausted_budget_says_so_and_says_how_to_fix_it() {
        let mut p = empty_pack();
        p["budget_exceeded"] = json!(true);
        p["budget"] = json!(200);
        let out = render_human(&p);
        assert!(out.contains("200 chars exhausted"));
        assert!(out.contains("--budget <bigger>"));
    }

    #[test]
    fn flagged_and_failed_memories_carry_the_same_prefixes_the_recall_hook_uses() {
        let mut p = empty_pack();
        p["memories"] = json!([{
            "id": "a", "kb": "m", "title": "the bad idea",
            "score": 0.1, "salience": 0.5, "flagged": true, "warns": true, "drift_open": 3,
        }]);
        p["memories_total"] = json!(1);
        let out = render_human(&p);
        assert!(out.contains("⚠ disputed: ✗ didn't work: the bad idea"));
        assert!(out.contains("[⚠ 3 drift-flagged citation(s)]"));
    }

    #[test]
    fn more_line_is_absent_when_nothing_is_hidden() {
        assert!(more_line(3, 3).is_none());
        assert!(more_line(0, 0).is_none());
        // Never negative, never a panic, even if a lane somehow over-returns.
        assert!(more_line(1, 5).is_none());
    }

    #[test]
    fn the_renderer_never_prints_a_transcript_field() {
        // R0/R1/R3 guard: the pack shape carries no transcript, and the
        // renderer must not invent a reader for one.
        let mut p = empty_pack();
        p["sessions"] = json!([{
            "session_id": "s-1", "kb": "sessions", "display_name": "n",
            "started_at": 1, "age_days": 0, "stale": false,
            "commit_count": 0, "error_count": 0, "same_cwd": false,
            "transcript": "SHOULD NEVER BE RENDERED",
        }]);
        p["sessions_total"] = json!(1);
        let out = render_human(&p);
        assert!(!out.contains("SHOULD NEVER BE RENDERED"));
        assert!(out.contains("kb sessions read <id>"));
    }
}
