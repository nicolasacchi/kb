//! `coverage.simplecov` — SimpleCov's `.resultset.json` (V72-H4a).
//!
//! Two on-disk shapes, both still in the wild and both accepted:
//!
//! ```json
//! { "RSpec": { "coverage": { "/repo/a.rb": { "lines": [null, 1, 0] } },
//!              "timestamp": 1756000000 } }
//! { "RSpec": { "coverage": { "/repo/a.rb": [null, 1, 0] } } }
//! ```
//!
//! The array is per SOURCE LINE, 0-indexed: `null` = not relevant (blank,
//! comment, `end`), a number = how many times the line ran. Several suites
//! may appear at the top level; their coverage is MERGED by summing hits,
//! which is SimpleCov's own merge semantics.
//!
//! Output: one `coverage` fact per RELEVANT line (`{hits}`), and one
//! `coverage_summary` per file (`{covered, total, pct}` over relevant
//! lines only — the same denominator SimpleCov reports, so the number
//! beside a file matches the number the operator already knows).
//! `branches` is read only far enough to record whether the resultset
//! carried any; branch facts are not minted in this unit and the value
//! says so rather than implying a branch percentage nobody computed.

use super::{ParsedFact, ParsedRun};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub const TOOL: &str = "simplecov";

/// Parse a `.resultset.json`. `strip_prefix` is the root the TOOL saw.
pub fn parse(text: &str, strip_prefix: Option<&str>) -> Result<ParsedRun, String> {
    let root: Value = serde_json::from_str(text).map_err(|e| format!("parse resultset: {e}"))?;
    let obj = root
        .as_object()
        .ok_or_else(|| "resultset is not a JSON object".to_string())?;

    let mut run = ParsedRun::new(TOOL);
    // Merged per file: line index -> Option<hits>. `None` stays `None`
    // until some suite reports the line as relevant.
    let mut merged: BTreeMap<String, Vec<Option<i64>>> = BTreeMap::new();
    let mut had_branches = false;
    let mut skipped: Vec<String> = Vec::new();

    for (suite, body) in obj {
        let Some(body) = body.as_object() else {
            skipped.push(format!("{suite}: not an object"));
            continue;
        };
        if let Some(ts) = body.get("timestamp").and_then(|v| v.as_i64()) {
            run.produced_at = Some(run.produced_at.map_or(ts, |cur: i64| cur.max(ts)));
        }
        let Some(cov) = body.get("coverage").and_then(|v| v.as_object()) else {
            skipped.push(format!("{suite}: no coverage map"));
            continue;
        };
        for (raw_path, entry) in cov {
            let rel = match super::relativize(raw_path, strip_prefix) {
                Ok(r) => r,
                Err(reason) => {
                    skipped.push(reason);
                    continue;
                }
            };
            let lines = match entry {
                Value::Array(a) => a.as_slice(),
                Value::Object(o) => {
                    if o.get("branches")
                        .and_then(|b| b.as_array())
                        .is_some_and(|b| !b.is_empty())
                    {
                        had_branches = true;
                    }
                    match o.get("lines").and_then(|v| v.as_array()) {
                        Some(a) => a.as_slice(),
                        None => {
                            skipped.push(format!("{rel}: entry has no `lines` array"));
                            continue;
                        }
                    }
                }
                _ => {
                    skipped.push(format!("{rel}: coverage entry is neither array nor object"));
                    continue;
                }
            };
            let slot = merged.entry(rel).or_default();
            if slot.len() < lines.len() {
                slot.resize(lines.len(), None);
            }
            for (i, v) in lines.iter().enumerate() {
                match v {
                    Value::Null => {}
                    Value::Number(n) => {
                        let hits = n.as_i64().unwrap_or(0);
                        slot[i] = Some(slot[i].unwrap_or(0) + hits);
                    }
                    // SimpleCov writes `"ignored"` for `:nocov:` regions in
                    // some versions — relevant to a human, not a hit count.
                    _ => {}
                }
            }
        }
    }

    run.inspected = merged.keys().cloned().collect();
    for (path, lines) in merged {
        let mut covered = 0usize;
        let mut total = 0usize;
        for (i, hits) in lines.iter().enumerate() {
            let Some(hits) = hits else { continue };
            total += 1;
            if *hits > 0 {
                covered += 1;
            }
            run.facts.push(ParsedFact {
                path: path.clone(),
                blob_sha: None,
                range_start: Some(i as u32 + 1),
                range_end: Some(i as u32 + 1),
                kind: "coverage".to_string(),
                value: json!({ "hits": hits }),
                severity: None,
            });
        }
        let pct = if total == 0 {
            // An honest null, not a 0.0 that reads as "nothing covered".
            Value::Null
        } else {
            json!(((covered as f64 / total as f64) * 10_000.0).round() / 100.0)
        };
        run.facts.push(ParsedFact {
            path,
            blob_sha: None,
            range_start: None,
            range_end: None,
            kind: "coverage_summary".to_string(),
            value: json!({
                "covered": covered,
                "total": total,
                "pct": pct,
                "branches_present": had_branches,
                "branch_facts": false,
            }),
            severity: None,
        });
    }

    run.notes = skipped;
    Ok(run)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../../tests/fixtures/lanes/simplecov.resultset.json");

    #[test]
    fn the_fixture_parses_into_the_golden_fact_set() {
        let run = parse(FIXTURE, Some("/repo")).expect("parses");
        assert_eq!(run.tool, "simplecov");
        assert_eq!(run.produced_at, Some(1_756_000_000));
        assert!(run.notes.is_empty(), "{:?}", run.notes);

        let rendered: Vec<String> = run
            .facts
            .iter()
            .map(|f| {
                format!(
                    "{} {}:{} {}",
                    f.kind,
                    f.path,
                    f.range_start.map(|l| l.to_string()).unwrap_or("-".into()),
                    f.value
                )
            })
            .collect();
        assert_eq!(
            rendered,
            vec![
                "coverage app/models/widget.rb:2 {\"hits\":1}",
                "coverage app/models/widget.rb:3 {\"hits\":0}",
                "coverage app/models/widget.rb:5 {\"hits\":3}",
                "coverage app/models/widget.rb:6 {\"hits\":0}",
                "coverage_summary app/models/widget.rb:- {\"branch_facts\":false,\"branches_present\":false,\"covered\":2,\"pct\":50.0,\"total\":4}",
                "coverage lib/legacy.rb:2 {\"hits\":2}",
                "coverage lib/legacy.rb:3 {\"hits\":2}",
                "coverage_summary lib/legacy.rb:- {\"branch_facts\":false,\"branches_present\":false,\"covered\":2,\"pct\":100.0,\"total\":2}",
            ]
        );
    }

    #[test]
    fn two_suites_merge_by_summing_hits() {
        let text = r#"{
          "A": {"coverage": {"a.rb": [1, 0, null]}},
          "B": {"coverage": {"a.rb": [0, 2, null]}}
        }"#;
        let run = parse(text, None).expect("parses");
        let hits: Vec<i64> = run
            .facts
            .iter()
            .filter(|f| f.kind == "coverage")
            .map(|f| f.value["hits"].as_i64().unwrap())
            .collect();
        assert_eq!(hits, vec![1, 2]);
        let summary = run
            .facts
            .iter()
            .find(|f| f.kind == "coverage_summary")
            .unwrap();
        assert_eq!(summary.value["covered"], 2);
        assert_eq!(summary.value["total"], 2);
    }

    #[test]
    fn a_path_outside_the_prefix_is_a_named_skip_not_a_mis_attribution() {
        let text = r#"{"A": {"coverage": {"/elsewhere/a.rb": [1]}}}"#;
        let run = parse(text, Some("/repo")).expect("parses");
        assert!(run.facts.is_empty());
        assert_eq!(run.notes.len(), 1);
        assert!(run.notes[0].contains("/elsewhere/a.rb"), "{:?}", run.notes);
    }

    #[test]
    fn a_file_with_no_relevant_lines_reports_a_null_pct_never_zero() {
        let text = r#"{"A": {"coverage": {"a.rb": [null, null]}}}"#;
        let run = parse(text, None).expect("parses");
        assert_eq!(run.facts.len(), 1);
        assert_eq!(run.facts[0].kind, "coverage_summary");
        assert!(run.facts[0].value["pct"].is_null());
    }

    #[test]
    fn malformed_json_is_an_error_not_an_empty_run() {
        assert!(parse("{not json", None).is_err());
        assert!(parse("[]", None).is_err());
    }
}
