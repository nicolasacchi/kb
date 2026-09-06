//! `rubocop` — `rubocop --format json` (V72-H4a).
//!
//! ```json
//! { "metadata": { "rubocop_version": "1.66.1" },
//!   "files": [ { "path": "app/x.rb", "offenses": [
//!       { "severity": "convention", "message": "…", "cop_name": "Style/…",
//!         "correctable": true, "corrected": false,
//!         "location": { "start_line": 2, "last_line": 2 } } ] } ] }
//! ```
//!
//! One `diagnostic` fact per offense. The cop name, the message and
//! whether RuboCop believes it can autocorrect ride the value; the
//! severity is normalised into the shared closed vocabulary with
//! RuboCop's own word kept beside it (`severity_raw`) — `convention` and
//! `refactor` are RuboCop-specific rungs that no other tool emits, and
//! throwing them away to fit four names would be a lossy rollup.
//!
//! An offense with no `start_line` is SKIPPED and named: RuboCop always
//! reports one, so its absence means a payload this parser does not
//! understand, and inventing line 1 for it would be a wrong line on a
//! gutter.

use super::{normalize_severity, ParsedFact, ParsedRun};
use serde_json::{json, Value};

pub const TOOL: &str = "rubocop";

pub fn parse(text: &str, strip_prefix: Option<&str>) -> Result<ParsedRun, String> {
    let root: Value = serde_json::from_str(text).map_err(|e| format!("parse rubocop json: {e}"))?;
    let mut run = ParsedRun::new(TOOL);
    run.tool_version = root
        .get("metadata")
        .and_then(|m| m.get("rubocop_version"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let files = root
        .get("files")
        .and_then(|f| f.as_array())
        .ok_or_else(|| "rubocop json has no `files` array".to_string())?;

    for file in files {
        let Some(raw_path) = file.get("path").and_then(|p| p.as_str()) else {
            run.notes.push("a file entry has no `path`".to_string());
            continue;
        };
        let rel = match super::relativize(raw_path, strip_prefix) {
            Ok(r) => r,
            Err(reason) => {
                run.notes.push(reason);
                continue;
            }
        };
        run.inspected.push(rel.clone());
        let offenses = file
            .get("offenses")
            .and_then(|o| o.as_array())
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        for off in offenses {
            let loc = off.get("location");
            let start = loc
                .and_then(|l| l.get("start_line").or_else(|| l.get("line")))
                .and_then(|v| v.as_u64());
            let Some(start) = start else {
                run.notes
                    .push(format!("{rel}: an offense has no start_line — skipped"));
                continue;
            };
            let end = loc
                .and_then(|l| l.get("last_line"))
                .and_then(|v| v.as_u64())
                .unwrap_or(start)
                .max(start);
            let raw_sev = off
                .get("severity")
                .and_then(|v| v.as_str())
                .unwrap_or("warning");
            let cop = off
                .get("cop_name")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            run.facts.push(ParsedFact {
                path: rel.clone(),
                blob_sha: None,
                range_start: Some(start as u32),
                range_end: Some(end as u32),
                kind: "diagnostic".to_string(),
                value: json!({
                    "cop": cop,
                    "message": off.get("message").and_then(|v| v.as_str()).unwrap_or(""),
                    "correctable": off.get("correctable").and_then(|v| v.as_bool()).unwrap_or(false),
                    "corrected": off.get("corrected").and_then(|v| v.as_bool()).unwrap_or(false),
                    "severity_raw": raw_sev,
                    "source": TOOL,
                }),
                severity: Some(normalize_severity(raw_sev).to_string()),
            });
        }
    }
    Ok(run)
}

/// Build `diagnostic` facts from `GET /api/diagnostics`' `diagnostics/1`
/// rows (the `--from-lip` path).
///
/// This is the ONE place a lane fact may carry `blob_sha` from the tool:
/// kb-lip hashes the on-disk bytes before AND after every LSP round trip
/// and refuses on mismatch, and `lip::verify_blob_freshness` re-checks it
/// daemon-side — so the blob the caller pairs with these rows is one a
/// guard actually proved. The CLI still brackets the whole round with its
/// own before/after read of the same blob (see `kb-code lanes ingest
/// rubocop --from-lip`), because the daemon's guard covers the LSP call
/// and not the gap between two of the CLI's own HTTP requests.
///
/// LSP severities are the numeric 1–4 scale; anything else becomes
/// `warning`, which is what an unlabelled diagnostic is.
pub fn from_lip_diagnostics(path: &str, blob_sha: &str, rows: &[Value]) -> Vec<ParsedFact> {
    rows.iter()
        .filter_map(|d| {
            let line = d.get("line").and_then(|v| v.as_u64())? as u32;
            let end = d
                .get("end_line")
                .and_then(|v| v.as_u64())
                .unwrap_or(line as u64)
                .max(line as u64) as u32;
            let raw_sev = match d.get("severity").and_then(|v| v.as_i64()) {
                Some(1) => "error",
                Some(2) => "warning",
                Some(3) => "info",
                Some(4) => "hint",
                _ => "warning",
            };
            let code = d
                .get("code")
                .map(|c| match c {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .unwrap_or_default();
            Some(ParsedFact {
                path: path.to_string(),
                blob_sha: Some(blob_sha.to_string()),
                range_start: Some(line),
                range_end: Some(end),
                kind: "diagnostic".to_string(),
                value: json!({
                    "cop": code,
                    "message": d.get("message").and_then(|v| v.as_str()).unwrap_or(""),
                    "correctable": false,
                    "corrected": false,
                    "severity_raw": raw_sev,
                    "source": d.get("source").and_then(|v| v.as_str()).unwrap_or("lsp"),
                    "via": "lip",
                }),
                severity: Some(normalize_severity(raw_sev).to_string()),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../../tests/fixtures/lanes/rubocop.json");

    #[test]
    fn the_fixture_parses_into_the_golden_fact_set() {
        let run = parse(FIXTURE, None).expect("parses");
        assert_eq!(run.tool, "rubocop");
        assert_eq!(run.tool_version.as_deref(), Some("1.66.1"));
        assert!(run.notes.is_empty(), "{:?}", run.notes);
        let rendered: Vec<String> = run
            .facts
            .iter()
            .map(|f| {
                format!(
                    "{}:{}-{} {} {}",
                    f.path,
                    f.range_start.unwrap(),
                    f.range_end.unwrap(),
                    f.severity.as_deref().unwrap(),
                    f.value["cop"].as_str().unwrap()
                )
            })
            .collect();
        assert_eq!(
            rendered,
            vec![
                "app/models/widget.rb:2-2 info Style/StringLiterals",
                "app/models/widget.rb:5-5 warning Lint/UselessAssignment",
            ]
        );
        assert_eq!(run.facts[0].value["severity_raw"], "convention");
        assert_eq!(run.facts[0].value["correctable"], true);
        assert!(run.facts[0].blob_sha.is_none());
    }

    #[test]
    fn an_offense_with_no_line_is_a_named_skip_not_line_one() {
        let text = r#"{"files":[{"path":"a.rb","offenses":[
            {"severity":"error","message":"m","cop_name":"C","location":{}}]}]}"#;
        let run = parse(text, None).expect("parses");
        assert!(run.facts.is_empty());
        assert_eq!(run.notes.len(), 1);
        assert!(run.notes[0].contains("start_line"), "{:?}", run.notes);
    }

    #[test]
    fn lip_rows_are_the_only_facts_that_may_carry_a_tool_named_blob() {
        let rows = vec![json!({
            "line": 4, "col": 0, "end_line": 4, "end_col": 3,
            "severity": 2, "code": "Style/Foo", "source": "rubocop", "message": "m"
        })];
        let facts = from_lip_diagnostics("a.rb", "deadbeef", &rows);
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].blob_sha.as_deref(), Some("deadbeef"));
        assert_eq!(facts[0].severity.as_deref(), Some("warning"));
        assert_eq!(facts[0].value["via"], "lip");
    }

    #[test]
    fn a_lip_row_with_no_line_is_dropped_rather_than_anchored_at_zero() {
        let rows = vec![json!({"message": "m"})];
        assert!(from_lip_diagnostics("a.rb", "deadbeef", &rows).is_empty());
    }

    #[test]
    fn a_payload_without_files_is_an_error() {
        assert!(parse("{}", None).is_err());
    }
}
