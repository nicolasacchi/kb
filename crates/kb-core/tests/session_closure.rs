//! W0 (sessions-rethink) — the closure extractor's fixture smoke tests and
//! the offline extraction GATE.
//!
//! Two halves:
//!
//! 1. **Synthetic fixtures** (`tests/session_fixtures/synthetic-*.jsonl`) —
//!    hand-authored transcripts modeled on the three real session SHAPES the
//!    design maps catalogued (workflow-heavy / kb-command-heavy / trivial
//!    husk). Every string in them is invented: this repo is public and real
//!    transcripts are private, so no capture content is ever copied in.
//!    They pin that `parse_session_activity` survives the shapes and that
//!    `closing_assistant_text` lands where a human would point.
//!
//! 2. **The GATE** (`#[ignore]`d) — the memo's week-1 risk settlement: run the
//!    ONE extraction fn over EVERY capture in a real sessions corpus and emit
//!    a machine-readable report for human review, BEFORE the V0029 column and
//!    the digest change ship. Run it explicitly:
//!
//!    ```text
//!    KB_W0_SESSIONS_DIR=~/kb/sessions \
//!      cargo test -p kb-core --profile fast --test session_closure -- \
//!      --ignored --nocapture closure_extraction_gate
//!    ```
//!
//!    It writes one JSON object per capture to `KB_W0_REPORT_OUT`
//!    (default `/tmp/kb-w0-closure-gate.jsonl`) and prints the aggregates.
//!    It NEVER writes into the repo.

use kb_core::sessions::{
    closing_assistant_text, parse_session_activity, recover_jsonl_from_capture,
};

const SYNTHETIC_FIXTURES: &[&str] = &[
    "synthetic-workflow-heavy",
    "synthetic-kb-commands",
    "synthetic-husk",
];

fn fixture_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/session_fixtures")
        .join(format!("{name}.jsonl"))
}

fn fixture(name: &str) -> String {
    std::fs::read_to_string(fixture_path(name))
        .unwrap_or_else(|e| panic!("read fixture {name}: {e}"))
}

/// Every synthetic fixture must be valid JSONL — the same hard requirement
/// the real-transcript fixture suite pins for its own corpus.
#[test]
fn synthetic_fixtures_are_valid_jsonl() {
    for name in SYNTHETIC_FIXTURES {
        let raw = fixture(name);
        for (i, line) in raw.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            serde_json::from_str::<serde_json::Value>(line)
                .unwrap_or_else(|e| panic!("{name}:{}: not valid JSON: {e}", i + 1));
        }
    }
}

/// The whole pipeline entry point must survive each shape without panicking,
/// and report the counts the fixture was built to carry.
#[test]
fn synthetic_fixtures_parse_without_panicking() {
    let a = parse_session_activity(&fixture("synthetic-workflow-heavy"));
    assert_eq!(a.message_count, 40);
    assert_eq!(
        a.session_id.as_deref(),
        Some("7c1f0a52-3b8e-4d17-9a60-2e5c8f41bd03")
    );
    assert_eq!(a.cwd.as_deref(), Some("/home/user/project/demo"));
    assert_eq!(
        a.first_user_prompt.as_deref(),
        Some(
            "Build the atlas fit workflow with three parallel builders and report back when every lane is green."
        ),
        "the typed pass must beat the caveat + /model command wrappers"
    );
    assert!(
        a.commits.iter().any(|c| c.kind == "commit"),
        "the workflow fixture carries a real git commit"
    );

    let b = parse_session_activity(&fixture("synthetic-kb-commands"));
    assert_eq!(b.message_count, 24);
    assert!(
        b.research.iter().any(|r| r.kind == "kb_search"),
        "the kb-command fixture's `kb search` call is a research signal"
    );

    let c = parse_session_activity(&fixture("synthetic-husk"));
    assert_eq!(c.message_count, 6);
    assert_eq!(
        c.first_user_prompt, None,
        "a husk has no prompt behind its wrappers"
    );
    assert_eq!(c.tool_calls, 0);
}

/// R3 — the closure each shape should yield.
#[test]
fn synthetic_fixtures_yield_the_expected_closure() {
    // A: the invented closing summary, NOT the ghost thinking fragment that
    // follows it (the requestId-shatter tail).
    let a = closing_assistant_text(&fixture("synthetic-workflow-heavy"));
    assert_eq!(
        a.as_deref(),
        Some(
            "Done. All three lanes finished: recon mapped the fit surfaces, the build lane landed \
             the viewport clamp in fit.rs, and the verify lane re-ran the layout goldens - 12 of \
             12 green. Committed as 4f1c2ab on main."
        )
    );

    // B: the substantial closing wins over the later terse "Tests green."
    // one-liner, and the session's last record is a tool_result.
    let b = closing_assistant_text(&fixture("synthetic-kb-commands"));
    assert_eq!(
        b.as_deref(),
        Some(
            "Recorded the ruling in memory as 4c1d9a77bb21 and left the code untouched - the cap \
             of three attempts with a parked batch afterwards is still what both prior decisions \
             say."
        )
    );

    // C: a husk closes nothing.
    assert_eq!(closing_assistant_text(&fixture("synthetic-husk")), None);
}

// ---------------------------------------------------------------------------
// THE GATE — offline extraction validation over a real capture corpus.
// ---------------------------------------------------------------------------

/// How a capture's transcript ENDS — the tail shape the extractor had to reach
/// back through. Computed from the last non-empty JSONL record.
fn ending_kind(jsonl: &str) -> &'static str {
    let Some(last) = jsonl.lines().rfind(|l| !l.trim().is_empty()) else {
        return "empty";
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(last) else {
        return "unparseable";
    };
    let role = v
        .get("message")
        .and_then(|m| m.get("role"))
        .and_then(|x| x.as_str());
    let blocks: Vec<&str> = v
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|b| b.get("type").and_then(|x| x.as_str()))
                .collect()
        })
        .unwrap_or_default();
    let has = |k: &str| blocks.contains(&k);
    match role {
        Some("assistant") if has("text") => "assistant-text",
        Some("assistant") if has("tool_use") => "assistant-tool_use",
        Some("assistant") if has("thinking") => "assistant-thinking",
        Some("assistant") => "assistant-other",
        Some("user") if has("tool_result") => "user-tool_result",
        Some("user") => "user-text",
        _ => match v.get("type").and_then(|x| x.as_str()) {
            Some("attachment") => "attachment",
            Some(_) => "meta",
            None => "unknown",
        },
    }
}

/// Diagnostic ONLY (never the shipped rule): the last assistant record with
/// any non-empty text block, with NO skips at all. Measures how much the
/// typed rule + substantial pass actually move the answer.
fn naive_tail_text(jsonl: &str) -> Option<String> {
    let mut out = None;
    for line in jsonl.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        let Some(msg) = v.get("message") else {
            continue;
        };
        if msg.get("role").and_then(|x| x.as_str()) != Some("assistant") {
            continue;
        }
        let text = match msg.get("content") {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(serde_json::Value::Array(blocks)) => blocks
                .iter()
                .filter(|b| b.get("type").and_then(|x| x.as_str()) == Some("text"))
                .filter_map(|b| b.get("text").and_then(|x| x.as_str()))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => continue,
        };
        if !text.trim().is_empty() {
            out = Some(text.trim().to_string());
        }
    }
    out
}

/// Records between the chosen closure and the end of the transcript — how far
/// back the extractor had to reach.
fn distance_from_end(jsonl: &str, text: &str) -> Option<usize> {
    let lines: Vec<&str> = jsonl.lines().filter(|l| !l.trim().is_empty()).collect();
    let head: String = text.chars().take(60).collect();
    lines
        .iter()
        .rposition(|l| l.contains(&head.replace('\\', "\\\\").replace('"', "\\\"")))
        .or_else(|| {
            // Fall back to a JSON-decoded search when the escaped-form probe
            // misses (unicode escapes, multi-block joins).
            lines.iter().rposition(|l| {
                serde_json::from_str::<serde_json::Value>(l)
                    .ok()
                    .and_then(|v| {
                        v.get("message")
                            .and_then(|m| m.get("content"))
                            .and_then(|c| c.as_array())
                            .map(|blocks| {
                                blocks.iter().any(|b| {
                                    b.get("text")
                                        .and_then(|x| x.as_str())
                                        .is_some_and(|t| t.contains(&head))
                                })
                            })
                    })
                    .unwrap_or(false)
            })
        })
        .map(|i| lines.len() - 1 - i)
}

#[test]
#[ignore = "offline GATE: needs KB_W0_SESSIONS_DIR pointing at a real capture corpus"]
fn closure_extraction_gate_over_real_captures() {
    let dir = std::env::var("KB_W0_SESSIONS_DIR")
        .expect("set KB_W0_SESSIONS_DIR to a directory of capture .html files");
    let out_path = std::env::var("KB_W0_REPORT_OUT")
        .unwrap_or_else(|_| "/tmp/kb-w0-closure-gate.jsonl".to_string());

    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read dir {dir}: {e}"))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("html"))
        .collect();
    files.sort();

    let mut out = String::new();
    let (mut parsed, mut hits, mut misses, mut no_pre) = (0usize, 0usize, 0usize, 0usize);
    let mut endings: std::collections::BTreeMap<&'static str, usize> = Default::default();
    let mut lens: Vec<usize> = Vec::new();
    let mut dists: Vec<usize> = Vec::new();
    let mut differs_from_naive = 0usize;

    for path in &files {
        let name = path
            .file_name()
            .and_then(|x| x.to_str())
            .unwrap_or("?")
            .to_string();
        let html = match std::fs::read_to_string(path) {
            Ok(h) => h,
            Err(e) => {
                out.push_str(&format!(
                    "{}\n",
                    serde_json::json!({"file": name, "error": e.to_string()})
                ));
                continue;
            }
        };
        let Some(jsonl) = recover_jsonl_from_capture(&html) else {
            no_pre += 1;
            out.push_str(&format!(
                "{}\n",
                serde_json::json!({"file": name, "parsed": false})
            ));
            continue;
        };
        parsed += 1;
        let records = jsonl.lines().filter(|l| !l.trim().is_empty()).count();
        let ending = ending_kind(&jsonl);
        *endings.entry(ending).or_default() += 1;
        let closure = closing_assistant_text(&jsonl);
        let naive = naive_tail_text(&jsonl);
        let mut row = serde_json::json!({
            "file": name,
            "parsed": true,
            "records": records,
            "ending": ending,
        });
        match &closure {
            Some(text) => {
                hits += 1;
                let chars = text.chars().count();
                lens.push(chars);
                let dist = distance_from_end(&jsonl, text);
                if let Some(d) = dist {
                    dists.push(d);
                }
                if naive.as_deref() != Some(text.as_str()) {
                    differs_from_naive += 1;
                }
                row["hit"] = serde_json::json!(true);
                row["chars"] = serde_json::json!(chars);
                row["dist_from_end"] = serde_json::json!(dist);
                row["differs_from_naive"] = serde_json::json!(naive.as_deref() != Some(text));
                row["text_head"] = serde_json::json!(text.chars().take(240).collect::<String>());
                row["naive_head"] = serde_json::json!(naive
                    .as_deref()
                    .map(|t| t.chars().take(120).collect::<String>()));
            }
            None => {
                misses += 1;
                row["hit"] = serde_json::json!(false);
                row["naive_head"] = serde_json::json!(naive
                    .as_deref()
                    .map(|t| t.chars().take(120).collect::<String>()));
            }
        }
        out.push_str(&format!("{row}\n"));
    }

    std::fs::write(&out_path, &out).unwrap_or_else(|e| panic!("write {out_path}: {e}"));

    lens.sort_unstable();
    dists.sort_unstable();
    let pct = |v: &[usize], p: usize| -> usize {
        if v.is_empty() {
            0
        } else {
            v[(v.len() - 1) * p / 100]
        }
    };
    println!("== W0 closure-extraction gate ==");
    println!("dir            {dir}");
    println!("report         {out_path}");
    println!("files          {}", files.len());
    println!("parsed         {parsed} (no <pre>: {no_pre})");
    println!(
        "extraction     hit {hits} / none {misses}  ({:.1}% hit)",
        100.0 * hits as f64 / parsed.max(1) as f64
    );
    println!(
        "chars          p50 {} · p90 {} · max {}",
        pct(&lens, 50),
        pct(&lens, 90),
        lens.last().copied().unwrap_or(0)
    );
    println!(
        "dist from end  p50 {} · p90 {} · max {}",
        pct(&dists, 50),
        pct(&dists, 90),
        dists.last().copied().unwrap_or(0)
    );
    println!("differs from naive tail  {differs_from_naive}");
    println!("endings        {endings:?}");

    assert!(parsed > 0, "no captures parsed — wrong KB_W0_SESSIONS_DIR?");
}
