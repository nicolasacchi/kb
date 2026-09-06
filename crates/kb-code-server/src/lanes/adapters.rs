//! Adapters — the three built-in parsers that turn a tool's own output
//! into `lane-ingest/1` facts (V72-H4a).
//!
//! These live in the SERVER crate and are called by `kb-code lanes
//! ingest`, which runs on the operator's box. That is deliberate and it is
//! not a violation of invariant 10: parsing a file the operator handed the
//! parser is not spawning a process. Keeping the parsers here gives ONE
//! home for the fact shape a lane writes (the registry's `fact_schema`
//! lives twenty lines away) and lets `ci-code`'s `cargo test -p
//! kb-code-server` golden-pin all three against checked-in fixtures.
//!
//! Every parser is TOTAL over well-formed JSON of its own format and
//! SKIPS LOUDLY otherwise: a result with no location, a coverage path
//! outside the declared root, an offense with no line — each is counted
//! and named in [`ParsedRun::notes`], never silently dropped and never
//! guessed onto line 1.

pub mod rubocop;
pub mod sarif;
pub mod simplecov;

use serde::{Deserialize, Serialize};

/// One fact on its way in, before the daemon attributes a blob. `path` is
/// repository-relative; the ingest route is what validates containment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParsedFact {
    pub path: String,
    /// `Some` only when the TOOL itself named the blob — which none of the
    /// three built-in formats does. `None` becomes `sha_source =
    /// mirror_at_ingest` at the daemon, which caps the fact at `likely`
    /// forever. That is the honest outcome, not a gap to paper over.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range_start: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range_end: Option<u32>,
    pub kind: String,
    pub value: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
}

/// One parsed tool run: what produced it, what it said, and what the
/// parser refused to guess at.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParsedRun {
    pub tool: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_version: Option<String>,
    /// The tool's own timestamp when it reports one (SimpleCov does);
    /// `None` means the CLI stamps ingest time instead and says so.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub produced_at: Option<i64>,
    pub facts: Vec<ParsedFact>,
    /// Human-readable, per-skip. Rendered by the CLI's run summary.
    #[serde(default)]
    pub notes: Vec<String>,
    /// Every path the tool INSPECTED, whether or not it had anything to
    /// say about it. The CLI sends the ones with no facts as the ingest's
    /// `clear_paths`, so a fixed offense stops being reported instead of
    /// living forever behind a re-run that simply did not mention the
    /// file. Empty when the format cannot express "I looked and found
    /// nothing" — SARIF is the case in point, and its adapter says so.
    #[serde(default)]
    pub inspected: Vec<String>,
}

impl ParsedRun {
    pub fn new(tool: &str) -> Self {
        Self {
            tool: tool.to_string(),
            tool_version: None,
            produced_at: None,
            facts: Vec::new(),
            notes: Vec::new(),
            inspected: Vec::new(),
        }
    }
}

/// The closed severity vocabulary a lane fact may carry. Every adapter
/// normalises into it AND keeps the tool's own word in the fact value
/// (`severity_raw`), so a rollup is comparable across tools without the
/// original ever being thrown away.
pub const SEVERITIES: &[&str] = &["error", "warning", "info", "hint"];

/// Map a tool's severity word onto [`SEVERITIES`]. Unknown words become
/// `info` — the neutral rung — rather than being dropped, because a
/// diagnostic with an unrecognised severity is still a diagnostic.
pub fn normalize_severity(raw: &str) -> &'static str {
    match raw.trim().to_ascii_lowercase().as_str() {
        "error" | "fatal" | "critical" | "high" | "blocker" => "error",
        "warning" | "warn" | "medium" | "moderate" => "warning",
        "note" | "info" | "information" | "informational" | "convention" | "low" => "info",
        "hint" | "refactor" | "style" | "none" => "hint",
        _ => "info",
    }
}

/// Turn a tool-reported path into a repository-relative one.
///
/// Three shapes arrive in practice: an absolute path from a run inside a
/// container (`/repo/app/x.rb`), a `file://` URI (SARIF), and an already
/// relative path (RuboCop's default). `strip_prefix` is the operator's
/// `--strip-prefix` — the root the TOOL saw, which is not necessarily the
/// root this daemon browses.
///
/// Returns `Err(reason)` rather than guessing when an absolute path does
/// not sit under the declared prefix: a coverage file produced somewhere
/// else entirely must be a named skip, not a silently mis-attributed one.
pub fn relativize(raw: &str, strip_prefix: Option<&str>) -> Result<String, String> {
    let raw = raw.trim();
    let without_scheme = raw.strip_prefix("file://").unwrap_or(raw);
    // A `file:` URI's authority is empty for a local path (`file:///a/b`),
    // so stripping the scheme leaves the absolute path intact.
    let candidate = without_scheme;
    if let Some(prefix) = strip_prefix.map(|p| p.trim_end_matches('/')) {
        if !prefix.is_empty() {
            if let Some(rest) = candidate.strip_prefix(prefix) {
                let rest = rest.trim_start_matches('/');
                if rest.is_empty() {
                    return Err(format!("{raw}: names the root itself, not a file"));
                }
                return Ok(rest.to_string());
            }
        }
    }
    if candidate.starts_with('/') {
        return Err(format!(
            "{raw}: absolute, and outside --strip-prefix (pass the root the tool saw)"
        ));
    }
    if candidate.is_empty() {
        return Err("empty path".to_string());
    }
    Ok(candidate.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_normalisation_is_closed() {
        for raw in [
            "error",
            "fatal",
            "warning",
            "convention",
            "refactor",
            "note",
            "none",
            "wat",
        ] {
            assert!(
                SEVERITIES.contains(&normalize_severity(raw)),
                "{raw} normalised outside the vocabulary"
            );
        }
        assert_eq!(normalize_severity("Fatal"), "error");
        assert_eq!(normalize_severity("convention"), "info");
        assert_eq!(normalize_severity("refactor"), "hint");
    }

    #[test]
    fn relativize_handles_the_three_real_shapes() {
        assert_eq!(
            relativize("/repo/app/x.rb", Some("/repo")).unwrap(),
            "app/x.rb"
        );
        assert_eq!(
            relativize("file:///repo/lib/y.rb", Some("/repo/")).unwrap(),
            "lib/y.rb"
        );
        assert_eq!(relativize("app/z.rb", None).unwrap(), "app/z.rb");
        assert_eq!(relativize("file://app/z.rb", None).unwrap(), "app/z.rb");
    }

    #[test]
    fn an_absolute_path_outside_the_prefix_is_a_named_skip_not_a_guess() {
        let e = relativize("/elsewhere/app/x.rb", Some("/repo")).unwrap_err();
        assert!(e.contains("--strip-prefix"), "{e}");
        let e = relativize("/abs/app/x.rb", None).unwrap_err();
        assert!(e.contains("absolute"), "{e}");
    }
}
