//! The `kb-term-commands/1` catalogue validator (CMDSCHEMA).
//!
//! A catalogue is the per-harness list of slash commands, key chords and
//! quick replies the terminal UI offers. This module is VALIDATE-ONLY: a
//! pure function over a parsed JSON value that returns every defect as an
//! ordered `{code, path}` list. It has no routes, no store and no wire
//! struct; later units own those.
//!
//! The SPA carries a twin validator (`web/src/terminal/commands/schema.ts`).
//! Both read ONE shared golden (`commands.golden.json`) and must agree on
//! every case, code AND path.
//!
//! Error order is deterministic and identical in both validators: an object's
//! known fields are visited in the fixed schema order, then its unknown fields
//! in sorted order; the keys of the `harnesses` map are visited sorted;
//! arrays are visited by index; depth first. A path is an RFC 6901 JSON
//! pointer into the document. Lengths are Unicode code points.

use std::collections::HashSet;

use serde_json::{Map, Value};

use super::keys_allow::is_named_key;

/// The catalogue's `schema` value.
pub const SCHEMA: &str = "kb-term-commands/1";

/// One defect: a stable code and the JSON pointer of the offending value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmdError {
    pub code: &'static str,
    pub path: String,
}

const MAX_GROUPS: usize = 20;
const MAX_ITEMS: usize = 200;
const MAX_ARGS: usize = 4;
const MAX_KEYS: usize = 8;
const MAX_LABEL: usize = 40;
const MAX_DESCRIPTION: usize = 200;
const MAX_TEXT: usize = 1000;
const MAX_HINT: usize = 60;
const MAX_DEFAULT: usize = 200;

/// Every code the validator can emit (the golden must cover each one).
#[cfg(test)]
const ALL_CODES: [&str; 19] = [
    "bad-type",
    "missing-field",
    "bad-schema",
    "unknown-harness",
    "unknown-field",
    "bad-id",
    "duplicate-id",
    "too-long",
    "empty",
    "too-many",
    "kind-shape",
    "bad-mode",
    "run-multiline",
    "control-char",
    "undeclared-arg",
    "unused-arg",
    "bad-arg",
    "bad-placeholder",
    "bad-key",
];

fn push(out: &mut Vec<CmdError>, code: &'static str, path: &str) {
    out.push(CmdError {
        code,
        path: path.to_string(),
    });
}

fn seg(base: &str, s: &str) -> String {
    format!("{base}/{}", s.replace('~', "~0").replace('/', "~1"))
}

fn idx(base: &str, i: usize) -> String {
    format!("{base}/{i}")
}

fn unknown_fields(m: &Map<String, Value>, known: &[&str], base: &str, out: &mut Vec<CmdError>) {
    let mut extra: Vec<&String> = m.keys().filter(|k| !known.contains(&k.as_str())).collect();
    extra.sort();
    for k in extra {
        push(out, "unknown-field", &seg(base, k));
    }
}

/// `^[a-z0-9][a-z0-9-]{0,47}$`
pub(crate) fn valid_id(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 48
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// `^[a-z][a-z0-9_]{0,23}$`
fn valid_arg_name(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 24
        && b[0].is_ascii_lowercase()
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
}

/// Required/optional string with a code-point length window.
fn check_str<'a>(
    v: Option<&'a Value>,
    path: &str,
    required: bool,
    allow_empty: bool,
    max: usize,
    out: &mut Vec<CmdError>,
) -> Option<&'a str> {
    match v {
        None => {
            if required {
                push(out, "missing-field", path);
            }
            None
        }
        Some(Value::String(s)) => {
            let n = s.chars().count();
            if n == 0 && !allow_empty {
                push(out, "empty", path);
            } else if n > max {
                push(out, "too-long", path);
            }
            Some(s.as_str())
        }
        Some(_) => {
            push(out, "bad-type", path);
            None
        }
    }
}

fn check_id(v: Option<&Value>, path: &str, seen: &mut HashSet<String>, out: &mut Vec<CmdError>) {
    match v {
        None => push(out, "missing-field", path),
        Some(Value::String(s)) => {
            if !valid_id(s) {
                push(out, "bad-id", path);
            } else if !seen.insert(s.clone()) {
                push(out, "duplicate-id", path);
            }
        }
        Some(_) => push(out, "bad-type", path),
    }
}

/// Validate a parsed catalogue. Never panics; any JSON value is accepted as
/// input and a non-conforming one yields errors.
pub fn validate(v: &Value) -> Vec<CmdError> {
    let mut out = Vec::new();
    let Some(root) = v.as_object() else {
        push(&mut out, "bad-type", "");
        return out;
    };
    match root.get("schema") {
        None => push(&mut out, "missing-field", "/schema"),
        Some(Value::String(s)) => {
            if s != SCHEMA {
                push(&mut out, "bad-schema", "/schema");
            }
        }
        Some(_) => push(&mut out, "bad-type", "/schema"),
    }
    match root.get("harnesses") {
        None => push(&mut out, "missing-field", "/harnesses"),
        Some(Value::Object(m)) => harnesses(m, &mut out),
        Some(_) => push(&mut out, "bad-type", "/harnesses"),
    }
    unknown_fields(root, &["schema", "harnesses"], "", &mut out);
    out
}

fn harnesses(m: &Map<String, Value>, out: &mut Vec<CmdError>) {
    let mut keys: Vec<&String> = m.keys().collect();
    keys.sort();
    for k in keys {
        let path = seg("/harnesses", k);
        if !kb_core::sessions::HARNESSES.contains(&k.as_str()) {
            push(out, "unknown-harness", &path);
            continue;
        }
        harness(&m[k], &path, out);
    }
}

fn harness(v: &Value, path: &str, out: &mut Vec<CmdError>) {
    let Some(h) = v.as_object() else {
        push(out, "bad-type", path);
        return;
    };
    let gpath = format!("{path}/groups");
    match h.get("groups") {
        None => push(out, "missing-field", &gpath),
        Some(Value::Array(groups)) => {
            if groups.len() > MAX_GROUPS {
                push(out, "too-many", &gpath);
            }
            let mut seen_groups = HashSet::new();
            let mut seen_items = HashSet::new();
            let mut total = 0usize;
            for (i, g) in groups.iter().enumerate() {
                group(
                    g,
                    &idx(&gpath, i),
                    &mut seen_groups,
                    &mut seen_items,
                    &mut total,
                    out,
                );
            }
            if total > MAX_ITEMS {
                push(out, "too-many", &gpath);
            }
        }
        Some(_) => push(out, "bad-type", &gpath),
    }
    unknown_fields(h, &["groups"], path, out);
}

fn group(
    v: &Value,
    path: &str,
    seen_groups: &mut HashSet<String>,
    seen_items: &mut HashSet<String>,
    total: &mut usize,
    out: &mut Vec<CmdError>,
) {
    let Some(g) = v.as_object() else {
        push(out, "bad-type", path);
        return;
    };
    check_id(g.get("id"), &seg(path, "id"), seen_groups, out);
    check_str(
        g.get("label"),
        &seg(path, "label"),
        true,
        false,
        MAX_LABEL,
        out,
    );
    let ipath = seg(path, "items");
    match g.get("items") {
        None => push(out, "missing-field", &ipath),
        Some(Value::Array(items)) => {
            *total += items.len();
            for (i, it) in items.iter().enumerate() {
                item(it, &idx(&ipath, i), seen_items, out);
            }
        }
        Some(_) => push(out, "bad-type", &ipath),
    }
    unknown_fields(g, &["id", "label", "items"], path, out);
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Command,
    Chord,
    Reply,
}

/// Scan `text` for `{name}` placeholders (`{{`/`}}` are literal braces).
/// Returns the error codes in text order and the set of names used.
fn scan_placeholders(
    text: &str,
    declared: &HashSet<String>,
) -> (Vec<&'static str>, HashSet<String>) {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut errs = Vec::new();
    let mut used = HashSet::new();
    let mut i = 0;
    while i < n {
        match chars[i] {
            '{' => {
                if chars.get(i + 1) == Some(&'{') {
                    i += 2;
                    continue;
                }
                let Some(rel) = chars[i + 1..].iter().position(|c| *c == '}') else {
                    errs.push("bad-placeholder");
                    break;
                };
                let end = i + 1 + rel;
                let name: String = chars[i + 1..end].iter().collect();
                if valid_arg_name(&name) {
                    if !declared.contains(&name) {
                        errs.push("undeclared-arg");
                    }
                    used.insert(name);
                } else {
                    errs.push("bad-placeholder");
                }
                i = end + 1;
            }
            '}' => {
                if chars.get(i + 1) == Some(&'}') {
                    i += 2;
                } else {
                    errs.push("bad-placeholder");
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    (errs, used)
}

fn item(v: &Value, path: &str, seen_items: &mut HashSet<String>, out: &mut Vec<CmdError>) {
    let Some(it) = v.as_object() else {
        push(out, "bad-type", path);
        return;
    };
    // kind first (parsed before the loop, reported at its own position).
    let kind = match it.get("kind") {
        Some(Value::String(s)) => match s.as_str() {
            "command" => Some(Kind::Command),
            "chord" => Some(Kind::Chord),
            "reply" => Some(Kind::Reply),
            _ => None,
        },
        _ => None,
    };
    let is_chord = kind == Some(Kind::Chord);
    let needs_text = matches!(kind, Some(Kind::Command) | Some(Kind::Reply));
    let run_mode = matches!(it.get("mode"), Some(Value::String(s)) if s == "run");

    // declared arg names (valid grammar only) and the placeholder scan.
    let mut declared = HashSet::new();
    if let Some(Value::Array(args)) = it.get("args") {
        for a in args {
            if let Some(Value::String(n)) = a.get("name") {
                if valid_arg_name(n) {
                    declared.insert(n.clone());
                }
            }
        }
    }
    let (scan_errs, used) = match it.get("text") {
        Some(Value::String(t)) if !is_chord => scan_placeholders(t, &declared),
        _ => (Vec::new(), HashSet::new()),
    };

    check_id(it.get("id"), &seg(path, "id"), seen_items, out);

    let kpath = seg(path, "kind");
    match it.get("kind") {
        None => push(out, "missing-field", &kpath),
        Some(Value::String(_)) => {
            if kind.is_none() {
                push(out, "kind-shape", &kpath);
            }
        }
        Some(_) => push(out, "bad-type", &kpath),
    }

    check_str(
        it.get("label"),
        &seg(path, "label"),
        true,
        false,
        MAX_LABEL,
        out,
    );
    check_str(
        it.get("description"),
        &seg(path, "description"),
        false,
        true,
        MAX_DESCRIPTION,
        out,
    );

    // text
    let tpath = seg(path, "text");
    match it.get("text") {
        None => {
            if needs_text {
                push(out, "kind-shape", &tpath);
            }
        }
        Some(_) if is_chord => push(out, "kind-shape", &tpath),
        Some(tv) => {
            if let Some(t) = check_str(Some(tv), &tpath, false, false, MAX_TEXT, out) {
                if run_mode {
                    if t.contains('\n') || t.contains('\r') {
                        push(out, "run-multiline", &tpath);
                    } else if t.chars().any(|c| c.is_control()) {
                        push(out, "control-char", &tpath);
                    }
                } else if t.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
                    push(out, "control-char", &tpath);
                }
                for code in &scan_errs {
                    push(out, code, &tpath);
                }
            }
        }
    }

    // mode
    let mpath = seg(path, "mode");
    match it.get("mode") {
        None => {}
        Some(_) if is_chord => push(out, "kind-shape", &mpath),
        Some(Value::String(s)) => {
            if s != "paste" && s != "run" {
                push(out, "bad-mode", &mpath);
            }
        }
        Some(_) => push(out, "bad-type", &mpath),
    }

    // args
    let apath = seg(path, "args");
    match it.get("args") {
        None => {}
        Some(_) if is_chord => push(out, "kind-shape", &apath),
        Some(Value::Array(args)) => {
            if args.len() > MAX_ARGS {
                push(out, "too-many", &apath);
            }
            let mut seen_names = HashSet::new();
            for (i, a) in args.iter().enumerate() {
                arg(a, &idx(&apath, i), &mut seen_names, &used, out);
            }
        }
        Some(_) => push(out, "bad-type", &apath),
    }

    // keys
    let keypath = seg(path, "keys");
    match it.get("keys") {
        None => {
            if is_chord {
                push(out, "kind-shape", &keypath);
            }
        }
        Some(_) if needs_text => push(out, "kind-shape", &keypath),
        Some(Value::Array(keys)) => {
            if keys.is_empty() {
                push(out, "empty", &keypath);
            } else if keys.len() > MAX_KEYS {
                push(out, "too-many", &keypath);
            }
            for (i, k) in keys.iter().enumerate() {
                match k {
                    Value::String(s) => {
                        if !is_named_key(s) {
                            push(out, "bad-key", &idx(&keypath, i));
                        }
                    }
                    _ => push(out, "bad-type", &idx(&keypath, i)),
                }
            }
        }
        Some(_) => push(out, "bad-type", &keypath),
    }

    // unverified
    if let Some(u) = it.get("unverified") {
        if !u.is_boolean() {
            push(out, "bad-type", &seg(path, "unverified"));
        }
    }

    unknown_fields(
        it,
        &[
            "id",
            "kind",
            "label",
            "description",
            "text",
            "mode",
            "args",
            "keys",
            "unverified",
        ],
        path,
        out,
    );
}

fn arg(
    v: &Value,
    path: &str,
    seen_names: &mut HashSet<String>,
    used: &HashSet<String>,
    out: &mut Vec<CmdError>,
) {
    let Some(a) = v.as_object() else {
        push(out, "bad-type", path);
        return;
    };
    let npath = seg(path, "name");
    let mut valid_name: Option<&str> = None;
    match a.get("name") {
        None => push(out, "missing-field", &npath),
        Some(Value::String(s)) => {
            if !valid_arg_name(s) {
                push(out, "bad-arg", &npath);
            } else {
                valid_name = Some(s.as_str());
                if !seen_names.insert(s.clone()) {
                    push(out, "bad-arg", &npath);
                }
            }
        }
        Some(_) => push(out, "bad-type", &npath),
    }
    check_str(
        a.get("hint"),
        &seg(path, "hint"),
        true,
        false,
        MAX_HINT,
        out,
    );
    check_str(
        a.get("default"),
        &seg(path, "default"),
        false,
        true,
        MAX_DEFAULT,
        out,
    );
    unknown_fields(a, &["name", "hint", "default"], path, out);
    if let Some(n) = valid_name {
        if !used.contains(n) {
            push(out, "unused-arg", path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const GOLDEN: &str = include_str!("commands.golden.json");
    const DEFAULTS: &str = include_str!("../../../../web/src/terminal/commands/defaults.json");
    const OMP_HOOK: &str = include_str!("../../../../plugins/kb-memory/hooks/kb-omp.ts");

    fn golden() -> Vec<Value> {
        let g: Value = serde_json::from_str(GOLDEN).expect("golden parses");
        g["cases"].as_array().expect("cases").clone()
    }

    fn got(doc: &Value) -> Vec<(String, String)> {
        validate(doc)
            .into_iter()
            .map(|e| (e.code.to_string(), e.path))
            .collect()
    }

    fn want(case: &Value) -> Vec<(String, String)> {
        case["errors"]
            .as_array()
            .expect("errors")
            .iter()
            .map(|e| {
                (
                    e["code"].as_str().unwrap().to_string(),
                    e["path"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    }

    #[test]
    fn golden_cases_agree() {
        let cases = golden();
        assert!(cases.len() >= 40, "golden has {} cases", cases.len());
        for c in &cases {
            assert_eq!(got(&c["doc"]), want(c), "case {}", c["name"]);
        }
    }

    #[test]
    fn every_error_code_has_a_golden_case() {
        let cases = golden();
        for code in ALL_CODES {
            assert!(
                cases.iter().any(|c| want(c).iter().any(|(k, _)| k == code)),
                "no golden case for {code}"
            );
        }
        // and the golden names no code the validator cannot emit
        for c in &cases {
            for (k, _) in want(c) {
                assert!(ALL_CODES.contains(&k.as_str()), "unknown code {k}");
            }
        }
    }

    #[test]
    fn shipped_defaults_validate_clean() {
        let doc: Value = serde_json::from_str(DEFAULTS).expect("defaults parse");
        assert_eq!(got(&doc), Vec::<(String, String)>::new());
    }

    #[test]
    fn shipped_defaults_flag_everything_unverified_but_the_extension_commands() {
        let doc: Value = serde_json::from_str(DEFAULTS).unwrap();
        let verified = ["kb-context", "kb-desk", "kb-slate", "kb-distill"];
        for name in verified {
            assert!(
                OMP_HOOK.contains(&format!("cmd(\"{name}\"")),
                "{name} missing in the omp hook"
            );
        }
        for (_, h) in doc["harnesses"].as_object().unwrap() {
            for g in h["groups"].as_array().unwrap() {
                for it in g["items"].as_array().unwrap() {
                    let id = it["id"].as_str().unwrap();
                    let kind = it["kind"].as_str().unwrap();
                    let flagged = it["unverified"] == json!(true);
                    if kind == "command" || kind == "chord" {
                        let is_verified = kind == "command" && verified.contains(&id);
                        assert_eq!(flagged, !is_verified, "{id}");
                    }
                }
            }
        }
    }

    #[test]
    fn never_panics_on_arbitrary_json() {
        let mut deep = json!(1);
        for _ in 0..100 {
            deep = json!({ "harnesses": deep });
        }
        let items: Vec<Value> = (0..10_000)
            .map(|i| json!({"id": format!("i{i}"), "kind": "reply", "label": "x", "text": "y"}))
            .collect();
        let big = json!({
            "schema": SCHEMA,
            "harnesses": {"claude": {"groups": [{"id": "g", "label": "g", "items": items}]}}
        });
        for v in [
            json!(null),
            json!(1),
            json!("s"),
            json!([]),
            json!([1, [2, [3]]]),
            json!({}),
            json!({"schema": 1, "harnesses": []}),
            json!({"schema": SCHEMA, "harnesses": {"claude": null}}),
            json!({"schema": SCHEMA, "harnesses": {"claude": {"groups": [null, 1, "x", []]}}}),
            json!({"schema": SCHEMA, "harnesses": {"claude": {"groups": [{"items": [null, {"args": [1, {"name": 3}], "keys": [1, null]}]}]}}}),
            deep,
            big.clone(),
        ] {
            let _ = validate(&v);
        }
        assert!(validate(&big)
            .iter()
            .any(|e| e.code == "too-many" && e.path == "/harnesses/claude/groups"));
    }

    #[test]
    fn chord_names_are_the_relay_list() {
        let cases = golden();
        let bad = cases
            .iter()
            .find(|c| c["name"] == "bad-key")
            .expect("bad-key case");
        assert_eq!(
            got(&bad["doc"]),
            vec![(
                "bad-key".to_string(),
                "/harnesses/claude/groups/0/items/0/keys/0".to_string()
            )]
        );
        assert!(!is_named_key("Escape"));
        assert!(is_named_key("Esc"));
        // every relay name is accepted as a chord key
        let keys: Vec<Value> = super::super::keys_allow::NAMED_KEYS
            .iter()
            .map(|k| json!(k))
            .collect();
        for chunk in keys.chunks(MAX_KEYS) {
            let doc = json!({"schema": SCHEMA, "harnesses": {"claude": {"groups": [
                {"id": "g", "label": "g", "items": [{"id": "c", "kind": "chord", "label": "c", "keys": chunk}]}
            ]}}});
            assert_eq!(got(&doc), Vec::<(String, String)>::new());
        }
    }

    #[test]
    fn codepoint_counting() {
        // 40 astral code points (4 UTF-8 bytes, 2 UTF-16 units each) fit a label.
        let ok = "\u{1F600}".repeat(40);
        let bad = "\u{1F600}".repeat(41);
        let mk = |l: &str| {
            json!({"schema": SCHEMA, "harnesses": {"claude": {"groups": [
                {"id": "g", "label": l, "items": []}
            ]}}})
        };
        assert!(validate(&mk(&ok)).is_empty());
        assert_eq!(validate(&mk(&bad))[0].code, "too-long");
    }
}
