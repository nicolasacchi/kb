//! `sarif.*` — ONE generic SARIF 2.1.0 adapter (V72-H4a).
//!
//! SARIF is the normal form every security/quality scanner already emits
//! (Brakeman, CodeQL, Semgrep, Trivy…), which is why the registry carries
//! ONE family template rather than a row per tool: the adapter is generic
//! and the LANE ID is what the operator declares (`sarif.brakeman`), so
//! two scanners never merge into one indistinguishable pile of facts.
//!
//! The mapping is deliberately shallow — `results[] → diagnostic`, one
//! fact per result:
//!
//! * `ruleId` → `value.rule`, plus the rule's `name`/`shortDescription`
//!   from `tool.driver.rules[]` when the run carries them;
//! * `level` → the shared severity vocabulary, falling back to the rule's
//!   `defaultConfiguration.level`, then to `warning` (SARIF's own default
//!   for a result with no level);
//! * `locations[0].physicalLocation` → path + `region.startLine/endLine`.
//!
//! Three things are SKIPPED and named rather than guessed: a result with
//! no location at all (a run-level finding, which has no line to sit on),
//! a location with no `startLine` (SARIF permits byte-offset-only regions
//! and this adapter mints no line from them), and an `artifactLocation`
//! that resolves outside the declared root. Multiple locations on one
//! result take the FIRST and say so in the note count — SARIF's later
//! locations are related sites, not additional independent findings.

use super::{normalize_severity, ParsedFact, ParsedRun};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub const TOOL: &str = "sarif";
pub const SUPPORTED_VERSION: &str = "2.1.0";

pub fn parse(text: &str, strip_prefix: Option<&str>) -> Result<ParsedRun, String> {
    let root: Value = serde_json::from_str(text).map_err(|e| format!("parse sarif: {e}"))?;
    let version = root.get("version").and_then(|v| v.as_str()).unwrap_or("");
    if version != SUPPORTED_VERSION {
        return Err(format!(
            "sarif version {version:?} is not supported (this adapter reads {SUPPORTED_VERSION} only)"
        ));
    }
    let runs = root
        .get("runs")
        .and_then(|r| r.as_array())
        .ok_or_else(|| "sarif has no `runs` array".to_string())?;

    let mut out = ParsedRun::new(TOOL);
    let mut extra_locations = 0usize;

    for sarif_run in runs {
        let driver = sarif_run.get("tool").and_then(|t| t.get("driver"));
        if let Some(name) = driver.and_then(|d| d.get("name")).and_then(|v| v.as_str()) {
            // The FIRST run's driver names the tool; a multi-run file
            // keeps the first and notes the rest rather than inventing a
            // composite name.
            if out.tool == TOOL {
                out.tool = name.to_string();
                out.tool_version = driver
                    .and_then(|d| d.get("version").or_else(|| d.get("semanticVersion")))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
            } else if out.tool != name {
                out.notes.push(format!(
                    "a second run reports tool {name:?}; kept the first"
                ));
            }
        }
        let mut rules: BTreeMap<String, (Option<String>, Option<String>)> = BTreeMap::new();
        if let Some(list) = driver
            .and_then(|d| d.get("rules"))
            .and_then(|r| r.as_array())
        {
            for rule in list {
                let Some(id) = rule.get("id").and_then(|v| v.as_str()) else {
                    continue;
                };
                let name = rule
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let level = rule
                    .get("defaultConfiguration")
                    .and_then(|c| c.get("level"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                rules.insert(id.to_string(), (name, level));
            }
        }

        let results = sarif_run
            .get("results")
            .and_then(|r| r.as_array())
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        for res in results {
            let rule_id = res
                .get("ruleId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let locations = res
                .get("locations")
                .and_then(|l| l.as_array())
                .map(|v| v.as_slice())
                .unwrap_or(&[]);
            let Some(first) = locations.first() else {
                out.notes.push(format!(
                    "{}: a result carries no location — skipped (run-level findings have no line)",
                    if rule_id.is_empty() {
                        "<no ruleId>"
                    } else {
                        &rule_id
                    }
                ));
                continue;
            };
            if locations.len() > 1 {
                extra_locations += locations.len() - 1;
            }
            let phys = first.get("physicalLocation");
            let raw_uri = phys
                .and_then(|p| p.get("artifactLocation"))
                .and_then(|a| a.get("uri"))
                .and_then(|v| v.as_str());
            let Some(raw_uri) = raw_uri else {
                out.notes
                    .push(format!("{rule_id}: location has no artifactLocation.uri"));
                continue;
            };
            let rel = match super::relativize(raw_uri, strip_prefix) {
                Ok(r) => r,
                Err(reason) => {
                    out.notes.push(reason);
                    continue;
                }
            };
            let region = phys.and_then(|p| p.get("region"));
            let Some(start) = region
                .and_then(|r| r.get("startLine"))
                .and_then(|v| v.as_u64())
            else {
                out.notes.push(format!(
                    "{rel}: region has no startLine — skipped (no line is minted from a byte offset)"
                ));
                continue;
            };
            let end = region
                .and_then(|r| r.get("endLine"))
                .and_then(|v| v.as_u64())
                .unwrap_or(start)
                .max(start);
            let rule = rules.get(&rule_id);
            let raw_level = res
                .get("level")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .or_else(|| rule.and_then(|(_, lvl)| lvl.clone()))
                .unwrap_or_else(|| "warning".to_string());
            out.facts.push(ParsedFact {
                path: rel,
                blob_sha: None,
                range_start: Some(start as u32),
                range_end: Some(end as u32),
                kind: "diagnostic".to_string(),
                value: json!({
                    "rule": rule_id,
                    "rule_name": rule.and_then(|(n, _)| n.clone()),
                    "message": res
                        .get("message")
                        .and_then(|m| m.get("text"))
                        .and_then(|v| v.as_str())
                        .unwrap_or(""),
                    "severity_raw": raw_level,
                    "source": out.tool,
                }),
                severity: Some(normalize_severity(&raw_level).to_string()),
            });
        }
    }

    // `inspected` stays EMPTY, deliberately. A SARIF run reports findings,
    // not the file set it scanned (`run.artifacts` is optional and, where
    // present, usually lists only files a result referenced) — so this
    // adapter cannot say "I looked at x.rb and it was clean", and claiming
    // it could would make a re-import silently delete facts another tool
    // is responsible for.
    if extra_locations > 0 {
        out.notes.push(format!(
            "{extra_locations} additional location(s) on multi-location results were not minted \
             as separate facts (SARIF's later locations are related sites, not findings)"
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../../tests/fixtures/lanes/results.sarif");

    #[test]
    fn the_fixture_parses_into_the_golden_fact_set() {
        let run = parse(FIXTURE, Some("/repo")).expect("parses");
        assert_eq!(run.tool, "ExampleScanner");
        assert_eq!(run.tool_version.as_deref(), Some("2.4.0"));
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
                    f.value["rule"].as_str().unwrap()
                )
            })
            .collect();
        assert_eq!(
            rendered,
            vec![
                "app/models/widget.rb:3-4 error EX001",
                "lib/legacy.rb:2-2 warning EX002",
            ]
        );
        assert_eq!(run.facts[0].value["rule_name"], "UnsafeEval");
        // The third result has no location at all and is a NAMED skip.
        assert_eq!(run.notes.len(), 1, "{:?}", run.notes);
        assert!(run.notes[0].contains("EX003"), "{:?}", run.notes);
    }

    #[test]
    fn an_unsupported_version_is_refused_rather_than_best_efforted() {
        let text = r#"{"version":"2.0.0","runs":[]}"#;
        let e = parse(text, None).unwrap_err();
        assert!(e.contains("2.1.0"), "{e}");
    }

    #[test]
    fn a_region_with_no_start_line_is_a_named_skip() {
        let text = r#"{"version":"2.1.0","runs":[{"tool":{"driver":{"name":"T"}},
          "results":[{"ruleId":"R","locations":[{"physicalLocation":{
            "artifactLocation":{"uri":"a.rb"},"region":{"byteOffset":10}}}]}]}]}"#;
        let run = parse(text, None).expect("parses");
        assert!(run.facts.is_empty());
        assert!(run.notes[0].contains("startLine"), "{:?}", run.notes);
    }

    #[test]
    fn a_result_with_no_level_defaults_to_warning_then_to_the_rules_default() {
        let text = r#"{"version":"2.1.0","runs":[{"tool":{"driver":{"name":"T",
          "rules":[{"id":"R","defaultConfiguration":{"level":"error"}}]}},
          "results":[{"ruleId":"R","locations":[{"physicalLocation":{
            "artifactLocation":{"uri":"a.rb"},"region":{"startLine":1}}}]}]}]}"#;
        let run = parse(text, None).expect("parses");
        assert_eq!(run.facts[0].severity.as_deref(), Some("error"));
        assert_eq!(run.facts[0].value["severity_raw"], "error");
    }
}
