//! Shared helpers for kb-code-server integration tests.
//!
//! Only byte-identical copies live here. Divergent `boot*` / `fixture_repo`
//! / `wait_for_indexed` implementations stay next to the tests that own them.

#![allow(dead_code)]

use std::path::Path;
use std::process::Command;

pub fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git -C {} {:?} failed: {}",
        dir.display(),
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

pub fn init_repo(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
}

// --- protocol registry drift helpers (v0.45 N8) --------------------------------
//
// The schemas in `<repo>/schemas/` are hand-written structural lints of the
// serde types (docs/protocols.md). These helpers let a test hand a value
// produced by REAL kb-code-server code to the registered schema, so renaming a
// field, or adding a required one, in Rust without moving the schema fails a
// named test. The schema files are read at test time; boon's loader is never
// pointed at a network.

use std::collections::BTreeSet;

pub fn schemas_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../schemas")
        .canonicalize()
        .expect("schemas/ directory at the repo root")
}

fn schema_file_value(id: &str) -> serde_json::Value {
    let path = schemas_root().join(format!("{}.schema.json", id.replace('/', "-")));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{id}: cannot read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{id}: schema is not JSON: {e}"))
}

/// Validate `value` against the registered schema `id`; `Err` carries every
/// failing instance location.
pub fn conformance(id: &str, value: &serde_json::Value) -> Result<(), String> {
    let schema = schema_file_value(id);
    let url = schema["$id"].as_str().expect("schema $id").to_string();
    let mut compiler = boon::Compiler::new();
    compiler.set_default_draft(boon::Draft::V2020_12);
    compiler
        .add_resource(&url, schema)
        .unwrap_or_else(|e| panic!("{id}: {e}"));
    let mut schemas = boon::Schemas::new();
    let idx = compiler
        .compile(&url, &mut schemas)
        .unwrap_or_else(|e| panic!("{id}: not a valid draft 2020-12 schema: {e}"));
    schemas.validate(value, idx).map_err(|e| format!("{e:#}"))
}

/// Panic unless `value` conforms to registered schema `id`.
pub fn assert_conforms(id: &str, value: &serde_json::Value) {
    if let Err(e) = conformance(id, value) {
        panic!(
            "real kb-code-server output no longer conforms to schemas/{}.schema.json ({id}):\n{e}\n\nvalue: {}",
            id.replace('/', "-"),
            serde_json::to_string_pretty(value).unwrap_or_default()
        );
    }
}

fn schema_at<'a>(schema: &'a serde_json::Value, pointer: &str) -> &'a serde_json::Value {
    schema
        .pointer(pointer)
        .unwrap_or_else(|| panic!("schema has no {pointer}"))
}

/// The `required` keys of the object schema at `pointer` (`""` = the root).
pub fn schema_required(id: &str, pointer: &str) -> BTreeSet<String> {
    let schema = schema_file_value(id);
    schema_at(&schema, &format!("{pointer}/required"))
        .as_array()
        .unwrap_or_else(|| panic!("{id}: {pointer}/required is not an array"))
        .iter()
        .map(|v| v.as_str().expect("required entry").to_string())
        .collect()
}

/// Every property key the object schema at `pointer` declares.
pub fn schema_properties(id: &str, pointer: &str) -> BTreeSet<String> {
    let schema = schema_file_value(id);
    schema_at(&schema, &format!("{pointer}/properties"))
        .as_object()
        .unwrap_or_else(|| panic!("{id}: {pointer}/properties is not an object"))
        .keys()
        .cloned()
        .collect()
}

/// The keys serde REQUIRES of `T`: drop each key of the fully populated
/// `sample` in turn and see whether `T` still deserializes.
pub fn serde_required_keys<T: serde::de::DeserializeOwned>(
    sample: &serde_json::Value,
) -> BTreeSet<String> {
    serde_json::from_value::<T>(sample.clone())
        .unwrap_or_else(|e| panic!("the full sample must deserialize first: {e}"));
    let mut required = BTreeSet::new();
    for key in sample.as_object().expect("sample is an object").keys() {
        let mut cut = sample.clone();
        cut.as_object_mut().unwrap().remove(key);
        if serde_json::from_value::<T>(cut).is_err() {
            required.insert(key.clone());
        }
    }
    required
}

/// Two-way drift check between a serde input type and its registered schema:
/// the keys serde requires equal the schema's `required`, and every key of
/// the full sample is a declared schema property.
pub fn assert_serde_matches_schema<T: serde::de::DeserializeOwned>(
    id: &str,
    pointer: &str,
    full_sample: &serde_json::Value,
) {
    assert_eq!(
        serde_required_keys::<T>(full_sample),
        schema_required(id, pointer),
        "{id} {pointer}: the keys serde requires and the schema's `required` disagree"
    );
    let declared = schema_properties(id, pointer);
    for key in full_sample.as_object().unwrap().keys() {
        assert!(
            declared.contains(key),
            "{id} {pointer}: the Rust type accepts `{key}` but the schema does not declare it"
        );
    }
}

/// Every key of the object `value` is declared by the schema at `pointer`.
pub fn assert_keys_declared(id: &str, pointer: &str, value: &serde_json::Value) {
    let declared = schema_properties(id, pointer);
    for key in value.as_object().expect("an object").keys() {
        assert!(
            declared.contains(key),
            "{id} {pointer}: real output carries `{key}` which the schema does not declare"
        );
    }
}
