//! `kb validate` (v0.45 N1) — check a plain-file or wire document against the
//! versioned protocol registry in `schemas/`.
//!
//! A PURE OFFLINE file operation: no daemon call, no storage, no network.
//! The schemas (and `schemas/index.json`) are embedded with `include_str!`, so
//! an installed binary validates without a repo checkout. `validate` never
//! edits a file (invariant #6: sidecars are only ever changed through routes
//! and CLI verbs).
//!
//! The schemas are hand-written STRUCTURAL LINTS of the Rust serde types,
//! not a second source of truth: when `kb validate` and the daemon disagree,
//! the daemon (serde) is right and the schema is the bug. The drift gate is
//! `crates/kb-cli/tests/schema_registry.rs`, which validates bytes produced
//! by the real kb-core code against these schemas.
//!
//! Exit codes: 0 conforms · 1 does not conform (JSON-pointer paths listed) ·
//! 2 usage (unknown or undetectable schema, unreadable file, no FILE).

use anyhow::Result;
use boon::{Compiler, Draft, SchemaIndex, Schemas, ValidationError};
use serde_json::{json, Value};
use std::io::Read;
use std::path::Path;

/// `schemas/index.json`: id -> kind, owner source file, carrier.
const INDEX_JSON: &str = include_str!("../../../../schemas/index.json");

/// Every JSON Schema file of the registry, keyed by contract id. The
/// `registry_and_embedded_schemas_agree` test pins this table to the index.
const EMBEDDED: &[(&str, &str)] = &[
    (
        "kb-comments/1",
        include_str!("../../../../schemas/kb-comments-1.schema.json"),
    ),
    (
        "kb-comments/2",
        include_str!("../../../../schemas/kb-comments-2.schema.json"),
    ),
    (
        "kb-list/1",
        include_str!("../../../../schemas/kb-list-1.schema.json"),
    ),
    (
        "kb-slate/1",
        include_str!("../../../../schemas/kb-slate-1.schema.json"),
    ),
    (
        "kb-proposal/1",
        include_str!("../../../../schemas/kb-proposal-1.schema.json"),
    ),
    (
        "kbc-findings/1",
        include_str!("../../../../schemas/kbc-findings-1.schema.json"),
    ),
    (
        "kbc-findings/2",
        include_str!("../../../../schemas/kbc-findings-2.schema.json"),
    ),
    (
        "kbc-review-context/1",
        include_str!("../../../../schemas/kbc-review-context-1.schema.json"),
    ),
    (
        "kbc-github-export/1",
        include_str!("../../../../schemas/kbc-github-export-1.schema.json"),
    ),
    (
        "kbc-cmd/1",
        include_str!("../../../../schemas/kbc-cmd-1.schema.json"),
    ),
    (
        "kbc-theme/1",
        include_str!("../../../../schemas/kbc-theme-1.schema.json"),
    ),
    (
        "kb-sibling/1",
        include_str!("../../../../schemas/kb-sibling-1.schema.json"),
    ),
    (
        "coderef/1",
        include_str!("../../../../schemas/coderef-1.schema.json"),
    ),
    (
        "coderef-feed/1",
        include_str!("../../../../schemas/coderef-feed-1.schema.json"),
    ),
    (
        "unified-inbox/1",
        include_str!("../../../../schemas/unified-inbox-1.schema.json"),
    ),
    (
        "kbc-claim/1",
        include_str!("../../../../schemas/kbc-claim-1.schema.json"),
    ),
    (
        "kb-capture-grok/1",
        include_str!("../../../../schemas/kb-capture-grok-1.schema.json"),
    ),
    (
        "kb-session-bundle/1",
        include_str!("../../../../schemas/kb-session-bundle-1.schema.json"),
    ),
];

/// The id of the one registered contract that is a text grammar, not JSON.
const RECALL_ID: &str = "kb-recall/1";
// The marker grammar constants are the reader's own (`kb_core::sessions::view`),
// not copies: a change to the accepted `pos` range or the marker framing moves
// this lint with it. A value outside the range is read as "unknown rank", which
// a lint reports as malformed.
use kb_core::sessions::view::{
    is_recall_marker_id, RECALL_MARKER_POS_RANGE as RECALL_POS_RANGE,
    RECALL_MARKER_PREFIX as RECALL_PREFIX, RECALL_MARKER_SUFFIX as RECALL_SUFFIX,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Json,
    Jsonl,
    /// Only the FIRST non-empty line is the contract (a transcript whose head
    /// line is an adapter record); the rest of the file is another format.
    JsonlHead,
    MdHeader,
    Text,
}

impl Kind {
    fn parse(s: &str) -> Option<Kind> {
        match s {
            "json" => Some(Kind::Json),
            "jsonl" => Some(Kind::Jsonl),
            "jsonl-head" => Some(Kind::JsonlHead),
            "md-header" => Some(Kind::MdHeader),
            "text" => Some(Kind::Text),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Kind::Json => "json",
            Kind::Jsonl => "jsonl",
            Kind::JsonlHead => "jsonl-head",
            Kind::MdHeader => "md-header",
            Kind::Text => "text",
        }
    }
}

#[derive(Clone, Debug)]
struct Entry {
    id: String,
    kind: Kind,
    owner: String,
    carrier: String,
}

/// One validation problem: where (JSON pointer and/or 1-based line) and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub line: Option<usize>,
    pub pointer: String,
    pub message: String,
}

impl Problem {
    fn render(&self) -> String {
        let loc = match (self.line, self.pointer.is_empty()) {
            (Some(n), true) => format!("line {n}"),
            (Some(n), false) => format!("line {n} {}", self.pointer),
            (None, true) => "(root)".to_string(),
            (None, false) => self.pointer.clone(),
        };
        format!("{loc}: {}", self.message)
    }
}

fn registry() -> Vec<Entry> {
    let v: Value = serde_json::from_str(INDEX_JSON)
        .expect("schemas/index.json is valid JSON (pinned by the registry tests)");
    let rows = v["schemas"]
        .as_array()
        .expect("schemas/index.json has a `schemas` array");
    rows.iter()
        .map(|e| Entry {
            id: e["id"].as_str().unwrap_or_default().to_string(),
            kind: Kind::parse(e["kind"].as_str().unwrap_or_default())
                .expect("schemas/index.json kind is one of json|jsonl|md-header|text"),
            owner: e["owner"].as_str().unwrap_or_default().to_string(),
            carrier: e["carrier"].as_str().unwrap_or_default().to_string(),
        })
        .collect()
}

fn registered_ids(reg: &[Entry]) -> String {
    reg.iter()
        .map(|e| e.id.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

// --- JSON Schema engine -----------------------------------------------------

/// Compile one schema document. Self-contained by construction: the only
/// resource registered is the schema itself, and boon's loader is never
/// pointed at a network.
fn compile(schema_json: &str) -> std::result::Result<(Schemas, SchemaIndex), String> {
    let value: Value =
        serde_json::from_str(schema_json).map_err(|e| format!("schema is not valid JSON: {e}"))?;
    let url = value
        .get("$id")
        .and_then(Value::as_str)
        .unwrap_or("https://kb.invalid/schema.json")
        .to_string();
    let mut compiler = Compiler::new();
    compiler.set_default_draft(Draft::V2020_12);
    compiler
        .add_resource(&url, value)
        .map_err(|e| e.to_string())?;
    let mut schemas = Schemas::new();
    let idx = compiler
        .compile(&url, &mut schemas)
        .map_err(|e| e.to_string())?;
    Ok((schemas, idx))
}

/// Flatten boon's error tree to its leaves; a leaf names the instance
/// location and the failed keyword.
fn collect(err: &ValidationError<'_, '_>, line: Option<usize>, out: &mut Vec<Problem>) {
    if err.causes.is_empty() {
        let p = Problem {
            line,
            pointer: err.instance_location.to_string(),
            message: err.kind.to_string(),
        };
        if !out.contains(&p) {
            out.push(p);
        }
    } else {
        for c in &err.causes {
            collect(c, line, out);
        }
    }
}

fn check_value(
    schemas: &Schemas,
    idx: SchemaIndex,
    v: &Value,
    line: Option<usize>,
    out: &mut Vec<Problem>,
) {
    if let Err(e) = schemas.validate(v, idx) {
        collect(&e, line, out);
    }
}

fn not_json(line: Option<usize>, e: &serde_json::Error) -> Problem {
    Problem {
        line,
        pointer: String::new(),
        message: format!("not valid JSON: {e}"),
    }
}

// --- detection ---------------------------------------------------------------

fn slate_shape(v: &Value) -> bool {
    v.is_object()
        && ["seq", "id", "at", "kind", "prov"]
            .iter()
            .all(|k| v.get(*k).is_some())
}

fn id_from_object(v: &Value) -> std::result::Result<String, String> {
    if let Some(s) = v.get("schema").and_then(Value::as_str) {
        return Ok(s.to_string());
    }
    // kb-sibling/1 rides GET /api/identity, which has no `schema` key.
    if let Some(s) = v.get("sibling_protocol").and_then(Value::as_str) {
        return Ok(s.to_string());
    }
    // A capture adapter's head record names its own contract in `adapter`.
    if v.get("type").and_then(Value::as_str) == Some("adapter-meta") {
        if let Some(s) = v.get("adapter").and_then(Value::as_str) {
            return Ok(s.to_string());
        }
    }
    if slate_shape(v) {
        return Ok("kb-slate/1".to_string());
    }
    Err(
        "the JSON has no string `schema`, `sibling_protocol` or adapter-meta `adapter` key and \
         does not look like a kb-slate/1 ledger line"
            .to_string(),
    )
}

/// `schema:` from a leading `---` front-matter block.
fn front_matter_schema(text: &str) -> Option<String> {
    let mut lines = text.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        if let Some(v) = line.strip_prefix("schema:") {
            return Some(v.trim().trim_matches(|c| c == '"' || c == '\'').to_string());
        }
    }
    None
}

/// The first `<!-- kb-list {...} -->` line (the same shape
/// `kb_core::lists` reads); `Some(Err)` when it is present but not JSON.
fn find_kb_list_header(text: &str) -> Option<std::result::Result<Value, String>> {
    for line in text.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("<!-- kb-list ") {
            let end = rest.find(" -->")?;
            return Some(
                serde_json::from_str::<Value>(rest[..end].trim())
                    .map_err(|e| format!("kb-list header is not valid JSON: {e}")),
            );
        }
    }
    None
}

fn detect(text: &str) -> std::result::Result<String, String> {
    let t = text.trim_start();
    if t.starts_with('{') {
        if let Ok(v) = serde_json::from_str::<Value>(t) {
            return id_from_object(&v);
        }
        // Not one JSON document: try the first line as a JSONL record.
        let first = t.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        return match serde_json::from_str::<Value>(first) {
            Ok(v) => id_from_object(&v),
            Err(e) => Err(format!("cannot parse the file as JSON or JSONL: {e}")),
        };
    }
    if let Some(id) = front_matter_schema(t) {
        return Ok(id);
    }
    if let Some(header) = find_kb_list_header(text) {
        return header.and_then(|v| id_from_object(&v));
    }
    if text.lines().any(|l| l.trim().starts_with("<!--kb-recall/")) {
        return Ok(RECALL_ID.to_string());
    }
    Err(
        "cannot tell which contract this file follows (no JSON `schema` key, \
         kb-list header, front-matter `schema:` or kb-recall marker); pass --schema <id>"
            .to_string(),
    )
}

// --- the text grammar --------------------------------------------------------

fn validate_recall(text: &str) -> Vec<Problem> {
    let mut problems = Vec::new();
    let mut seen = 0usize;
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if !line.starts_with("<!--kb-recall/") {
            continue;
        }
        let n = Some(i + 1);
        let problem = |message: &str| Problem {
            line: n,
            pointer: String::new(),
            message: message.to_string(),
        };
        let Some(body) = line
            .strip_prefix(RECALL_PREFIX)
            .and_then(|l| l.strip_suffix(RECALL_SUFFIX))
        else {
            problems.push(problem(
                "not a `<!--kb-recall/1 key=value ...-->` marker (wrong version or shape)",
            ));
            continue;
        };
        seen += 1;
        let mut kb = None;
        let mut id = None;
        let mut pos = None;
        for pair in body.split_ascii_whitespace() {
            let Some((key, value)) = pair.split_once('=') else {
                continue; // a bare token is ignored, as the reader ignores it
            };
            match key {
                "kb" => kb = Some(value),
                "id" => id = Some(value),
                "pos" => pos = Some(value),
                _ => {} // unknown pairs are ignored: the grammar is forward-compatible
            }
        }
        if kb.map(str::trim).unwrap_or_default().is_empty() {
            problems.push(problem("missing or empty `kb`"));
        }
        match id {
            None => problems.push(problem("missing `id`")),
            Some(v) => {
                if !is_recall_marker_id(v) {
                    problems.push(problem("`id` must be exactly 12 lowercase hex characters"));
                }
            }
        }
        if let Some(v) = pos {
            let ok = v
                .parse::<u32>()
                .map(|p| RECALL_POS_RANGE.contains(&p))
                .unwrap_or(false);
            if !ok {
                problems.push(problem("`pos` must be an integer in 1..=99"));
            }
        }
    }
    if seen == 0 && problems.is_empty() {
        problems.push(Problem {
            line: None,
            pointer: String::new(),
            message: "no `<!--kb-recall/1 ...-->` marker found".to_string(),
        });
    }
    problems
}

// --- validation --------------------------------------------------------------

/// A caller-supplied schema must be self-contained: every `$ref` /
/// `$dynamicRef` has to be a same-document fragment (`#...`). Anything else
/// would make the schema compiler resolve a resource (a local file or a URL)
/// the user never handed us, so it is refused before compiling.
fn reject_external_refs(schema_json: &str) -> std::result::Result<(), String> {
    fn walk(v: &Value, path: &str) -> std::result::Result<(), String> {
        match v {
            Value::Object(m) => {
                for (k, child) in m {
                    if k == "$ref" || k == "$dynamicRef" {
                        if let Some(r) = child.as_str() {
                            if !r.starts_with('#') {
                                return Err(format!(
                                    "schema {path}/{k} is {r:?}: only same-document refs (\"#...\") \
                                     are allowed in a --schema file"
                                ));
                            }
                        }
                    }
                    walk(child, &format!("{path}/{k}"))?;
                }
                Ok(())
            }
            Value::Array(a) => a
                .iter()
                .enumerate()
                .try_for_each(|(i, c)| walk(c, &format!("{path}/{i}"))),
            _ => Ok(()),
        }
    }
    match serde_json::from_str::<Value>(schema_json) {
        Ok(v) => walk(&v, ""),
        // compile() reports the parse error with its own wording
        Err(_) => Ok(()),
    }
}

fn validate_with(
    kind: Kind,
    schema_json: &str,
    text: &str,
) -> std::result::Result<Vec<Problem>, String> {
    let (schemas, idx) = compile(schema_json)?;
    let mut problems = Vec::new();
    match kind {
        Kind::Json => match serde_json::from_str::<Value>(text) {
            Ok(v) => check_value(&schemas, idx, &v, None, &mut problems),
            Err(e) => problems.push(not_json(None, &e)),
        },
        Kind::Jsonl => {
            for (i, l) in text.lines().enumerate() {
                if l.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<Value>(l) {
                    Ok(v) => check_value(&schemas, idx, &v, Some(i + 1), &mut problems),
                    Err(e) => problems.push(not_json(Some(i + 1), &e)),
                }
            }
        }
        Kind::JsonlHead => match text.lines().enumerate().find(|(_, l)| !l.trim().is_empty()) {
            Some((i, l)) => match serde_json::from_str::<Value>(l) {
                Ok(v) => check_value(&schemas, idx, &v, Some(i + 1), &mut problems),
                Err(e) => problems.push(not_json(Some(i + 1), &e)),
            },
            None => problems.push(Problem {
                line: None,
                pointer: String::new(),
                message: "the file is empty".to_string(),
            }),
        },
        Kind::MdHeader => {
            // A JSON list export validates as itself; a Markdown list by its
            // `<!-- kb-list {...} -->` provenance header.
            if text.trim_start().starts_with('{') {
                match serde_json::from_str::<Value>(text) {
                    Ok(v) => check_value(&schemas, idx, &v, None, &mut problems),
                    Err(e) => problems.push(not_json(None, &e)),
                }
            } else {
                match find_kb_list_header(text) {
                    Some(Ok(v)) => check_value(&schemas, idx, &v, None, &mut problems),
                    Some(Err(message)) => problems.push(Problem {
                        line: None,
                        pointer: String::new(),
                        message,
                    }),
                    None => problems.push(Problem {
                        line: None,
                        pointer: String::new(),
                        message: "no `<!-- kb-list {...} -->` header found".to_string(),
                    }),
                }
            }
        }
        Kind::Text => unreachable!("text contracts have no JSON Schema"),
    }
    Ok(problems)
}

/// Validate `text` against the registered contract `forced`, or against the
/// one it declares. `Err` is a usage problem (exit 2); `Ok` carries the
/// contract id and the problems found (empty = conforms).
fn validate_text(
    text: &str,
    forced: Option<&str>,
) -> std::result::Result<(String, Vec<Problem>), String> {
    let reg = registry();
    let id = match forced {
        Some(id) => id.to_string(),
        None => detect(text)?,
    };
    let Some(entry) = reg.iter().find(|e| e.id == id) else {
        return Err(format!(
            "unknown schema {id:?}; registered: {}",
            registered_ids(&reg)
        ));
    };
    if entry.kind == Kind::Text {
        return Ok((id, validate_recall(text)));
    }
    let Some((_, schema_json)) = EMBEDDED.iter().find(|(k, _)| *k == id) else {
        return Err(format!("internal: no embedded schema for {id}"));
    };
    let problems = validate_with(entry.kind, schema_json, text)?;
    Ok((id, problems))
}

// --- the verb ----------------------------------------------------------------

fn usage_exit(json: bool, message: &str) -> ! {
    if json {
        println!("{}", json!({ "ok": false, "error": message }));
    } else {
        eprintln!("kb validate: {message}");
    }
    std::process::exit(2);
}

fn read_input(file: &Path) -> std::result::Result<String, String> {
    if file.as_os_str() == "-" {
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .map_err(|e| format!("cannot read stdin: {e}"))?;
        return Ok(s);
    }
    std::fs::read_to_string(file).map_err(|e| format!("cannot read {}: {e}", file.display()))
}

fn print_list(json: bool) -> Result<()> {
    let reg = registry();
    if json {
        let rows: Vec<Value> = reg
            .iter()
            .map(|e| {
                json!({
                    "id": e.id,
                    "kind": e.kind.as_str(),
                    "owner": e.owner,
                    "carrier": e.carrier,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else {
        for e in &reg {
            println!("{:<22} {:<10} {}", e.id, e.kind.as_str(), e.carrier);
        }
    }
    Ok(())
}

pub fn run(file: Option<&Path>, schema: Option<&str>, json: bool, list: bool) -> Result<()> {
    if list {
        return print_list(json);
    }
    let Some(file) = file else {
        usage_exit(json, "a FILE argument is required (or --list)");
    };
    let text = match read_input(file) {
        Ok(t) => t,
        Err(m) => usage_exit(json, &m),
    };
    let label = file.display().to_string();
    let outcome = match schema {
        // `--schema path/to/x.json`: a caller-supplied JSON Schema, whole-file JSON.
        Some(s) if s.ends_with(".json") => {
            let schema_json = match std::fs::read_to_string(s) {
                Ok(t) => t,
                Err(e) => usage_exit(json, &format!("cannot read schema {s}: {e}")),
            };
            reject_external_refs(&schema_json)
                .and_then(|()| validate_with(Kind::Json, &schema_json, &text))
                .map(|p| (s.to_string(), p))
        }
        Some(id) => validate_text(&text, Some(id)),
        None => validate_text(&text, None),
    };
    let (id, problems) = match outcome {
        Ok(v) => v,
        Err(m) => usage_exit(json, &m),
    };
    if json {
        let rows: Vec<Value> = problems
            .iter()
            .map(|p| json!({ "line": p.line, "pointer": p.pointer, "message": p.message }))
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "ok": problems.is_empty(),
                "schema": id,
                "file": label,
                "problems": rows,
            }))?
        );
    } else if problems.is_empty() {
        println!("ok: {label} conforms to {id}");
    } else {
        eprintln!(
            "FAIL: {label} does not conform to {id} ({} problem{})",
            problems.len(),
            if problems.len() == 1 { "" } else { "s" }
        );
        for p in problems.iter().take(50) {
            eprintln!("  {}", p.render());
        }
        if problems.len() > 50 {
            eprintln!("  ... and {} more", problems.len() - 50);
        }
    }
    if !problems.is_empty() {
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_and_embedded_schemas_agree() {
        let reg = registry();
        for e in &reg {
            let embedded = EMBEDDED.iter().any(|(k, _)| *k == e.id);
            assert_eq!(
                embedded,
                e.kind != Kind::Text,
                "{}: exactly the non-text contracts carry an embedded schema",
                e.id
            );
        }
        for (k, _) in EMBEDDED {
            assert!(
                reg.iter().any(|e| e.id == *k),
                "{k} embedded but not indexed"
            );
        }
    }

    #[test]
    fn every_embedded_schema_compiles_and_declares_its_own_id() {
        for (id, json) in EMBEDDED {
            compile(json).unwrap_or_else(|e| panic!("{id}: {e}"));
            let v: Value = serde_json::from_str(json).unwrap();
            assert_eq!(
                v["$id"],
                format!("https://github.com/nicolasacchi/kb/schemas/{id}"),
                "{id}"
            );
        }
    }

    #[test]
    fn detect_reads_the_declared_contract() {
        assert_eq!(
            detect("{\"schema\":\"kb-comments/2\"}").unwrap(),
            "kb-comments/2"
        );
        assert_eq!(
            detect("{\"seq\":1,\"id\":\"e_000000000001\",\"at\":1,\"kind\":\"now\",\"prov\":{}}\n")
                .unwrap(),
            "kb-slate/1"
        );
        assert_eq!(
            detect("---\nschema: kbc-review/1\n---\nbody").unwrap(),
            "kbc-review/1"
        );
        assert_eq!(
            detect("# T\n\n<!-- kb-list {\"schema\":\"kb-list/1\"} -->\n").unwrap(),
            "kb-list/1"
        );
        assert_eq!(
            detect("- x\n<!--kb-recall/1 kb=a id=0123456789ab-->\n").unwrap(),
            RECALL_ID
        );
        assert!(detect("{\"no\":\"schema\"}").is_err());
        assert!(detect("just prose").is_err());
    }

    #[test]
    fn unknown_schema_is_a_usage_error_naming_the_registry() {
        let err = validate_text("{\"schema\":\"kb-comments/9\"}", None).unwrap_err();
        assert!(err.contains("unknown schema"), "{err}");
        assert!(err.contains("kb-comments/1"), "{err}");
    }

    #[test]
    fn recall_grammar_matches_the_reader() {
        let ok = "<!--kb-recall/1 kb=kb id=a1b2c3d4e5f6-->\n\
                  <!--kb-recall/1 kb=kb id=a1b2c3d4e5f6 pos=99 future=1 bare-->\n";
        assert!(validate_recall(ok).is_empty());
        for bad in [
            "<!--kb-recall/1 kb=kb id=a1b2c3d4e5f6 pos=100-->",
            "<!--kb-recall/1 kb=kb id=a1b2c3d4e5f6 pos=0-->",
            "<!--kb-recall/1 kb=kb-->",
            "<!--kb-recall/1 id=a1b2c3d4e5f6-->",
            "<!--kb-recall/1 kb=kb id=A1B2C3D4E5F6-->",
            "<!--kb-recall/1 kb=kb id=a1b2c3d4e5-->",
            "<!--kb-recall/2 kb=kb id=a1b2c3d4e5f6-->",
            "no marker at all",
        ] {
            assert!(!validate_recall(bad).is_empty(), "{bad}");
        }
    }

    #[test]
    fn user_schemas_may_not_reference_other_resources() {
        for bad in [
            r#"{"$ref":"other.json"}"#,
            r#"{"properties":{"a":{"$ref":"file:///etc/hostname"}}}"#,
            r#"{"allOf":[{"$ref":"https://example.invalid/s.json"}]}"#,
        ] {
            let err = reject_external_refs(bad).unwrap_err();
            assert!(err.contains("same-document"), "{bad}: {err}");
        }
        assert!(reject_external_refs(
            r##"{"$defs":{"x":{"type":"string"}},"properties":{"a":{"$ref":"#/$defs/x"}}}"##
        )
        .is_ok());
        // a property merely NAMED "$ref" is not a reference
        assert!(reject_external_refs(r#"{"properties":{"$ref":{"type":"string"}}}"#).is_ok());
    }

    #[test]
    fn recall_range_is_the_readers_own() {
        assert_eq!(RECALL_POS_RANGE, 1..=99);
        let top = format!(
            "{RECALL_PREFIX}kb=kb id=a1b2c3d4e5f6 pos={}{RECALL_SUFFIX}",
            RECALL_POS_RANGE.end()
        );
        let over = format!(
            "{RECALL_PREFIX}kb=kb id=a1b2c3d4e5f6 pos={}{RECALL_SUFFIX}",
            RECALL_POS_RANGE.end() + 1
        );
        assert!(validate_recall(&top).is_empty());
        assert!(!validate_recall(&over).is_empty());
    }

    #[test]
    fn recall_id_rule_is_the_readers_single_source() {
        for id in [
            "a1b2c3d4e5f6",
            "A1B2C3D4E5F6",
            "a1b2c3d4e5f",
            "a1b2c3d4e5f67",
            "g1b2c3d4e5f6",
            "",
            "0123456789ab",
        ] {
            let m = format!("{RECALL_PREFIX}kb=kb id={id}{RECALL_SUFFIX}");
            assert_eq!(
                validate_recall(&m).is_empty(),
                is_recall_marker_id(id),
                "validate and the reader disagree on id {id:?}"
            );
        }
    }

    #[test]
    fn a_failure_names_the_json_pointer() {
        let doc = "{\"schema\":\"kb-proposal/1\",\"id\":\"p_0123456789ab\",\
                   \"created_at\":\"soon\",\"title\":\"t\",\"body\":\"b\",\"source\":\"agent\"}";
        let (id, problems) = validate_text(doc, None).unwrap();
        assert_eq!(id, "kb-proposal/1");
        assert!(
            problems.iter().any(|p| p.pointer == "/created_at"),
            "{problems:?}"
        );
    }
}
