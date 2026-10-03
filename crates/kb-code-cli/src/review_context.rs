//! v0.44 F9b — `kb-code review context <REF> [--ps N] [--budget N] [--json]`
//! and `kb-code review explain-base <REF> [--json]`.
//!
//! `context` is `GET /api/reviews/{id}/context` (`kbc-review-context/1`): one
//! deterministic, token-budgeted bundle for the agent reviewing a patchset —
//! header, open human threads, findings, other reviews' disputes on the same
//! paths, what the author changed since the verdict, the reading order, the
//! change set and the patch in reading order. A LEADING PART of the full
//! content, never a summary; every cut is named under `omitted`. Without
//! `--json` the verb prints the bundle's text form (the same sections, as
//! prose and a trailing patch); with `--json` the versioned envelope.
//!
//! `explain-base` is `GET /api/reviews/{id}/explain-base`
//! (`kbc-review-base-explain/1`): how the review's base was chosen, from what
//! the review RECORDED.
//!
//! Both are computed by `kb_code_server::review_context`; these verbs add no
//! interpretation.

use crate::envelope::{self, NextArgv};
use crate::review_agent::{self, argv, AgentError, DEFAULT_DAEMON};
use clap::Args;
use serde_json::Value;

pub use kb_code_server::review_context::{CONTEXT_SCHEMA, EXPLAIN_BASE_SCHEMA};

#[derive(Args, Debug)]
pub struct ContextArgs {
    /// `<id>`, `<id>/ps<n>`, `pr:<N>` or `pr:<N>/ps<n>`.
    pub target: String,
    /// The patchset (default: the latest). Same as `<ref>/ps<n>`.
    #[arg(long)]
    pub ps: Option<i64>,
    /// Bundle size in tokens (about 4 bytes each). Default 20000.
    #[arg(long, value_name = "TOKENS")]
    pub budget: Option<u64>,
    /// Disambiguates `pr:<N>` when several repos have one.
    #[arg(long)]
    pub repo: Option<String>,
    #[arg(long, default_value = DEFAULT_DAEMON)]
    pub daemon: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct ExplainBaseArgs {
    /// `<id>` or `pr:<N>`.
    pub target: String,
    /// Disambiguates `pr:<N>` when several repos have one.
    #[arg(long)]
    pub repo: Option<String>,
    #[arg(long, default_value = DEFAULT_DAEMON)]
    pub daemon: String,
    #[arg(long)]
    pub json: bool,
}

/// `GET /api/reviews/{id}/context?ps=&budget=` (walked by main.rs's
/// dead-surface test).
pub fn review_context_request(
    ps: Option<i64>,
    budget: Option<u64>,
) -> (&'static str, Vec<(&'static str, String)>) {
    let mut q = Vec::new();
    if let Some(n) = ps {
        q.push(("ps", n.to_string()));
    }
    if let Some(b) = budget {
        q.push(("budget", b.to_string()));
    }
    (kb_code_server::review_context::CONTEXT_ROUTE.path, q)
}

/// `GET /api/reviews/{id}/explain-base` (same walk).
pub fn explain_base_request() -> (&'static str, Vec<(&'static str, String)>) {
    (
        kb_code_server::review_context::EXPLAIN_BASE_ROUTE.path,
        Vec::new(),
    )
}

/// The follow-ups a cut bundle suggests: re-run with a bigger budget when
/// something was cut, and the narrower reads for the cut sections. Pure.
pub fn context_next(body: &Value) -> Vec<NextArgv> {
    let id = body["review_id"].as_i64().unwrap_or(0).to_string();
    let cut: Vec<&str> = body["omitted"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|o| o["section"].as_str())
        .collect();
    let mut next = Vec::new();
    if cut.is_empty() {
        return next;
    }
    let tokens = body["budget"]["tokens"].as_u64().unwrap_or(0);
    next.push(argv(&[
        "kb-code",
        "review",
        "context",
        &id,
        "--budget",
        &(tokens.saturating_mul(2).max(1000)).to_string(),
        "--json",
    ]));
    if cut.contains(&"patch") {
        next.push(argv(&[
            "kb-code", "review", "diff", &id, "--patch", "--path", "<file>",
        ]));
    }
    if cut.contains(&"threads") {
        next.push(argv(&["kb-code", "review", "comments", &id, "--json"]));
    }
    if cut.contains(&"findings") {
        next.push(argv(&[
            "kb-code", "review", "findings", "list", &id, "--json",
        ]));
    }
    next
}

/// `review context --json`'s envelope. Pure.
pub fn context_envelope(body: &Value, addr: &str) -> Value {
    let next = context_next(body);
    let mut data = body.clone();
    data["ref"] = Value::String(addr.to_string());
    // A cut bundle is not a degraded one: `data.omitted` names every cut and
    // `next` says how to read more.
    envelope::ok_value(CONTEXT_SCHEMA, data, Vec::new(), false, None, next)
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}

/// The bundle as text: a header block, the sections as short lines, then the
/// patch verbatim. Pure and deterministic.
pub fn context_text(body: &Value) -> String {
    let mut out = String::new();
    let h = &body["header"];
    out.push_str(&format!(
        "# review {} ps{} - {}\n",
        body["review_id"],
        body["ps"],
        h["title"].as_str().unwrap_or("(untitled)")
    ));
    out.push_str(&format!(
        "{} <- {} - state {}\n",
        s(h, "head_ref"),
        s(h, "base_ref"),
        s(h, "state")
    ));
    if let Some(n) = h["pr_number"].as_i64() {
        out.push_str(&format!("PR #{n}\n"));
    }
    out.push_str(&format!(
        "base: {}\n",
        h["base_resolution"]["summary"]
            .as_str()
            .unwrap_or("unknown")
    ));
    if let Some(v) = h["verdict"].as_object() {
        out.push_str(&format!(
            "verdict: {} at ps{}{}\n",
            v.get("state").and_then(Value::as_str).unwrap_or("?"),
            v.get("ps").map(|p| p.to_string()).unwrap_or_default(),
            if h["verdict_stale"] == true {
                " (stale: a later patchset landed)"
            } else {
                ""
            }
        ));
    }
    if h["drift"]["head_moved"] == true {
        out.push_str("drift: the PR head moved past the latest patchset\n");
    }

    let threads = body["threads"].as_array().cloned().unwrap_or_default();
    out.push_str(&format!("\n## open threads ({})\n", threads.len()));
    for t in &threads {
        out.push_str(&format!(
            "- [{}] {} {}: {}\n",
            s(t, "intent"),
            s(t, "path"),
            s(t, "author"),
            s(t, "body").replace('\n', " ")
        ));
    }
    let findings = body["findings"].as_array().cloned().unwrap_or_default();
    out.push_str(&format!("\n## findings ({})\n", findings.len()));
    for f in &findings {
        out.push_str(&format!(
            "- {} [{}] {} - {}{}\n",
            s(f, "slug"),
            s(f, "severity"),
            s(f, "title"),
            f["disposition"].as_str().unwrap_or("undecided"),
            if f["resolution"]["orphaned"] == true {
                " (orphaned)"
            } else {
                ""
            }
        ));
    }
    let others = body["other_reviews"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    out.push_str(&format!(
        "\n## decided in other reviews, same paths ({})\n",
        others.len()
    ));
    for o in &others {
        out.push_str(&format!(
            "- review {} {} {}: {} ({})\n",
            o["review_id"],
            s(o, "path"),
            s(o, "slug"),
            s(o, "disposition"),
            o["note"].as_str().unwrap_or("no note")
        ));
    }
    out.push_str("\n## since the verdict\n");
    match body["since"]["applicable"].as_bool() {
        Some(true) => {
            let r = &body["since"]["report"];
            out.push_str(&format!(
                "ps{} -> ps{}: {} new / {} gone hunk(s), {}\n",
                r["from"]["ps"],
                r["to"]["ps"],
                r["author_delta"]["new_hunks"],
                r["author_delta"]["gone_hunks"],
                if r["rebase_only"] == true {
                    "rebase-only"
                } else {
                    "AUTHOR CHANGED"
                }
            ));
        }
        Some(false) => out.push_str(&format!("n/a ({})\n", s(&body["since"], "reason"))),
        None => out.push_str("(cut by the budget)\n"),
    }
    let order = body["reading_order"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    out.push_str(&format!("\n## reading order ({})\n", order.len()));
    for (i, st) in order.iter().enumerate() {
        out.push_str(&format!(
            "{}. {} - {}\n",
            i + 1,
            s(st, "path"),
            s(st, "reason")
        ));
    }
    let omitted = body["omitted"].as_array().cloned().unwrap_or_default();
    if !omitted.is_empty() {
        out.push_str("\n## omitted (budget)\n");
        for o in &omitted {
            out.push_str(&format!(
                "- {}: kept {} of {}\n",
                s(o, "section"),
                o["kept"],
                o["total"]
            ));
        }
    }
    for r in body["patch"]["redacted"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "\n(redacted by policy: {} matches {})\n",
            s(r, "path"),
            s(r, "pattern")
        ));
    }
    out.push_str("\n## patch (reading order)\n");
    out.push_str(s(&body["patch"], "text"));
    out
}

/// `review explain-base --json`'s envelope. Pure.
pub fn explain_envelope(body: &Value, addr: &str) -> Value {
    let mut data = body.clone();
    data["ref"] = Value::String(addr.to_string());
    envelope::ok_value(
        EXPLAIN_BASE_SCHEMA,
        data,
        Vec::new(),
        false,
        None,
        Vec::new(),
    )
}

/// The chain as lines. Pure.
pub fn explain_lines(body: &Value) -> Vec<String> {
    let mut out = vec![format!(
        "review {} base: {}",
        body["review_id"],
        s(body, "summary")
    )];
    for r in body["chain"].as_array().into_iter().flatten() {
        out.push(format!(
            "  {:<12} {:<16} {}",
            s(r, "status"),
            s(r, "rung"),
            s(r, "label")
        ));
    }
    for w in body["warnings"].as_array().into_iter().flatten() {
        out.push(format!("  warning {}: {}", s(w, "code"), s(w, "message")));
    }
    for p in body["patchsets"].as_array().into_iter().flatten() {
        out.push(format!(
            "  ps{} {} tip {}",
            p["ps"],
            p["kind"].as_str().unwrap_or("(legacy, kind not recorded)"),
            s(p, "tip_sha").get(..12).unwrap_or("")
        ));
    }
    out
}

pub async fn context_cmd(a: ContextArgs) -> anyhow::Result<()> {
    let json = a.json;
    match context_run(&a).await {
        Ok(()) => Ok(()),
        Err(e) => e.emit(json),
    }
}

async fn context_run(a: &ContextArgs) -> Result<(), AgentError> {
    let res = review_agent::resolve(&a.daemon, &a.target, a.repo.as_deref(), a.ps).await?;
    let (tpl, q) = review_context_request(res.ps, a.budget);
    let body = review_agent::get_ok(
        &a.daemon,
        &review_agent::fill_id(tpl, res.id),
        &q,
        "review context",
    )
    .await?;
    if a.json {
        envelope::print_value(&context_envelope(&body, &a.target));
        return Ok(());
    }
    print!("{}", context_text(&body));
    Ok(())
}

pub async fn explain_base_cmd(a: ExplainBaseArgs) -> anyhow::Result<()> {
    let json = a.json;
    match explain_base_run(&a).await {
        Ok(()) => Ok(()),
        Err(e) => e.emit(json),
    }
}

async fn explain_base_run(a: &ExplainBaseArgs) -> Result<(), AgentError> {
    review_agent::reject_patchset_address(&a.target, "explain-base")?;
    let res = review_agent::resolve(&a.daemon, &a.target, a.repo.as_deref(), None).await?;
    let (tpl, q) = explain_base_request();
    let body = review_agent::get_ok(
        &a.daemon,
        &review_agent::fill_id(tpl, res.id),
        &q,
        "review explain-base",
    )
    .await?;
    if a.json {
        envelope::print_value(&explain_envelope(&body, &a.target));
        return Ok(());
    }
    for l in explain_lines(&body) {
        println!("{l}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use serde_json::json;

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        a: ContextArgs,
    }

    fn bundle(cut: bool) -> Value {
        json!({
            "schema": CONTEXT_SCHEMA,
            "review_id": 7, "ps": 2,
            "budget": {"tokens": 1000},
            "header": {
                "title": "t", "head_ref": "feature", "base_ref": "main", "state": "open",
                "pr_number": 12,
                "base_resolution": {"summary": "tracks `main` on the forge"},
                "verdict": {"state": "approve", "ps": 1}, "verdict_stale": true,
                "drift": {"head_moved": false},
            },
            "threads": [{"intent": "question", "path": "a.rb", "author": "you", "body": "why?\nreally"}],
            "findings": [{"slug": "f-x", "severity": "concern", "title": "magic", "disposition": null,
                          "resolution": {"orphaned": true}}],
            "other_reviews": [],
            "since": {"applicable": true, "report": {"from": {"ps": 1}, "to": {"ps": 2},
                      "author_delta": {"new_hunks": 1, "gone_hunks": 0}, "rebase_only": false}},
            "reading_order": [{"path": "a.rb", "reason": "no dependency signal"}],
            "files": [],
            "patch": {"text": "diff --git a/a.rb b/a.rb\n+x\n", "redacted": [{"path": ".env", "pattern": ".env"}]},
            "omitted": if cut { json!([{"section": "patch", "kept": 0, "total": 2, "reason": "budget"}]) } else { json!([]) },
        })
    }

    #[test]
    fn requests_name_the_declared_routes() {
        let (path, q) = review_context_request(Some(3), Some(5000));
        assert_eq!(path, "/api/reviews/{id}/context");
        assert_eq!(
            q,
            vec![("ps", "3".to_string()), ("budget", "5000".to_string())]
        );
        assert!(review_context_request(None, None).1.is_empty());
        assert_eq!(explain_base_request().0, "/api/reviews/{id}/explain-base");
    }

    #[test]
    fn flags_parse() {
        let a = Cli::try_parse_from(["context", "pr:7/ps2", "--budget", "8000", "--json"])
            .unwrap()
            .a;
        assert_eq!((a.budget, a.json), (Some(8000), true));
    }

    #[test]
    fn text_is_deterministic_and_carries_every_section() {
        let t = context_text(&bundle(true));
        assert_eq!(t, context_text(&bundle(true)));
        for needle in [
            "# review 7 ps2 - t",
            "PR #12",
            "base: tracks `main` on the forge",
            "verdict: approve at ps1 (stale: a later patch",
            "## open threads (1)",
            "- [question] a.rb you: why? really",
            "f-x [concern] magic - undecided (orphaned)",
            "1 new / 0 gone hunk(s), AUTHOR CHANGED",
            "1. a.rb - no dependency signal",
            "- patch: kept 0 of 2",
            "(redacted by policy: .env matches .env)",
            "## patch (reading order)\ndiff --git a/a.rb b/a.rb\n+x\n",
        ] {
            assert!(t.contains(needle), "missing {needle:?} in:\n{t}");
        }
    }

    #[test]
    fn a_cut_bundle_suggests_a_bigger_budget_and_the_narrow_reads() {
        assert!(context_next(&bundle(false)).is_empty());
        let next = context_next(&bundle(true));
        assert_eq!(
            next[0],
            vec!["kb-code", "review", "context", "7", "--budget", "2000", "--json"]
        );
        assert!(next.iter().any(|a| a.contains(&"diff".to_string())));
        let env = context_envelope(&bundle(true), "pr:12");
        assert_eq!(env["schema"], CONTEXT_SCHEMA);
        assert_eq!(env["data"]["omitted"][0]["section"], "patch");
        assert_eq!(env["degraded"], false);
        assert_eq!(env["data"]["ref"], "pr:12");
    }

    #[test]
    fn explain_lines_show_the_chain_and_each_patchset() {
        let body = json!({
            "review_id": 7, "summary": "tracks `main`",
            "chain": [{"rung": "explicit", "label": "an explicit --base", "status": "missed"},
                      {"rung": "forge-api", "label": "the PR target", "status": "applied"}],
            "warnings": [{"code": "pr-target-assumed", "message": "m"}],
            "patchsets": [{"ps": 1, "kind": null, "tip_sha": "0123456789abcdef0123"},
                          {"ps": 2, "kind": "rebase", "tip_sha": "fedcba9876543210ffff"}],
        });
        let lines = explain_lines(&body);
        assert_eq!(lines[0], "review 7 base: tracks `main`");
        assert!(lines[1].contains("missed") && lines[1].contains("explicit"));
        assert!(lines[2].contains("applied") && lines[2].contains("forge-api"));
        assert!(lines[3].contains("pr-target-assumed"));
        assert!(lines[4].contains("legacy, kind not recorded"));
        assert!(lines[5].contains("rebase") && lines[5].contains("fedcba987654"));
    }
}
