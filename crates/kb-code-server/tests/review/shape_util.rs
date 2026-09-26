//! RS-U11a — the wire-SHAPE reduction + additive-diff helpers golden
//! envelope tests use, and that a later unit (U11 itself, which actually
//! grows the envelopes per README.md §12: `base{…}`, `warnings[]`,
//! `minted`, per-patchset `kind`/`base_tip_sha`) can reuse verbatim to
//! prove its change is additive-only against the SAME committed fixtures.
//!
//! # Why a "shape," not the raw response
//!
//! A byte-for-byte golden (`rs_u0_golden.rs`'s own approach) is the right
//! tool for pinning a DETERMINISTIC snapshot tool's output. These routes
//! carry wall-clock timestamps, random ids and real git shas — normalizing
//! every one of those away one field at a time would be its own multi-file
//! maintenance burden, and would still only prove "this exact value is
//! unchanged," which is the WRONG question here. The question RS-U11a
//! exists to answer is narrower and more durable: "does every key path
//! this envelope carried before the change still carry a value of the same
//! JSON TYPE after it." [`shape_of`] reduces a response to exactly that —
//! every leaf value replaced by a type tag (`"string"`, `"number"`,
//! `"bool"`, `"null"`), objects keep their keys, arrays keep only their
//! FIRST element's shape (this crate's envelopes are homogeneous arrays
//! throughout — a patchset list, a files list, a rows list — so one
//! element's shape stands for the whole). [`assert_additive`] then walks a
//! COMMITTED shape (the golden) against a fresh RAW response and requires
//! every golden key path to still resolve, at the same type, in the fresh
//! response — never the reverse, so a brand-new key the real U11 change
//! adds (`base`, `warnings`, `minted`, `kind`, `base_tip_sha`, …) is
//! invisible to this check by construction. A golden leaf of `"null"`
//! (an `Option` field that happened to be `None` in the fixture that
//! minted the golden) only requires the key to still be PRESENT — not
//! tightened to a specific type — because there is no real type recorded
//! for it to check.

/// Reduce a response body to its wire SHAPE — see the module doc.
pub(crate) fn shape_of(v: &serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Null => serde_json::Value::String("null".to_string()),
        serde_json::Value::Bool(_) => serde_json::Value::String("bool".to_string()),
        serde_json::Value::Number(_) => serde_json::Value::String("number".to_string()),
        serde_json::Value::String(_) => serde_json::Value::String("string".to_string()),
        serde_json::Value::Array(items) => match items.first() {
            Some(first) => serde_json::Value::Array(vec![shape_of(first)]),
            None => serde_json::Value::Array(Vec::new()),
        },
        serde_json::Value::Object(map) => {
            // Sorted by key regardless of the `serde_json/preserve_order`
            // feature, so the committed fixture's key order never depends
            // on that build-time choice or on `json!` macro call-site
            // field order — a stable, reviewable diff on regeneration.
            let mut entries: Vec<(&String, serde_json::Value)> =
                map.iter().map(|(k, val)| (k, shape_of(val))).collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            let mut out = serde_json::Map::with_capacity(entries.len());
            for (k, val) in entries {
                out.insert(k.clone(), val);
            }
            serde_json::Value::Object(out)
        }
    }
}

/// The JSON type tag `assert_additive` compares against a golden leaf.
fn type_tag(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// Walk `golden` (a [`shape_of`] output) against `actual` (an ORDINARY raw
/// JSON value — a fresh response body, NOT itself shape-reduced) and
/// collect every key path where the golden's shape no longer resolves.
/// An empty return means every golden key path still exists at the same
/// type — additive-only, by this check's own definition. See the module
/// doc for the full contract, including the deliberate one-way walk (only
/// `golden`'s keys are visited — a new key in `actual` is never an error)
/// and the `"null"` leaf's presence-only rule.
pub(crate) fn assert_additive(
    golden: &serde_json::Value,
    actual: &serde_json::Value,
    path: &str,
) -> Vec<String> {
    let mut errs = Vec::new();
    match golden {
        serde_json::Value::Object(gmap) => {
            let serde_json::Value::Object(amap) = actual else {
                errs.push(format!(
                    "{}: golden is an object, actual is {}",
                    display_path(path),
                    type_tag(actual)
                ));
                return errs;
            };
            for (k, gval) in gmap {
                let child = child_path(path, k);
                match amap.get(k) {
                    None => errs.push(format!("{child}: key removed")),
                    Some(aval) => errs.extend(assert_additive(gval, aval, &child)),
                }
            }
        }
        serde_json::Value::Array(garr) => {
            let serde_json::Value::Array(aarr) = actual else {
                errs.push(format!(
                    "{}: golden is an array, actual is {}",
                    display_path(path),
                    type_tag(actual)
                ));
                return errs;
            };
            // Element-shape can only be checked when the golden recorded
            // one AND the fresh response actually has an element to check
            // it against — an empty `actual` array is not itself a type
            // error (this route may legitimately return zero rows for
            // this particular fixture run).
            if let (Some(gfirst), Some(afirst)) = (garr.first(), aarr.first()) {
                errs.extend(assert_additive(gfirst, afirst, &format!("{path}[]")));
            }
        }
        serde_json::Value::String(tag) if tag == "null" => {
            // Presence-only — see the module doc.
        }
        serde_json::Value::String(tag) => {
            let atag = type_tag(actual);
            if atag != tag {
                errs.push(format!(
                    "{}: golden type {tag:?}, actual type {atag:?}",
                    display_path(path)
                ));
            }
        }
        other => errs.push(format!(
            "{}: golden fixture itself is malformed (not a shape_of output): {other}",
            display_path(path)
        )),
    }
    errs
}

fn child_path(parent: &str, key: &str) -> String {
    if parent.is_empty() {
        key.to_string()
    } else {
        format!("{parent}.{key}")
    }
}

fn display_path(path: &str) -> &str {
    if path.is_empty() {
        "<root>"
    } else {
        path
    }
}

/// Deterministic ordering helper for the one test below that asserts on a
/// multi-problem message list (`assert_additive`'s own output order
/// follows the golden object's iteration order, which is not itself part
/// of its contract).
#[cfg(test)]
fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn shape_of_reduces_every_leaf_to_a_type_tag_and_sorts_object_keys() {
        let v = json!({"b": 1, "a": "x", "c": null, "d": true, "e": [1, 2, 3]});
        let shape = shape_of(&v);
        assert_eq!(
            shape,
            json!({"a": "string", "b": "number", "c": "null", "d": "bool", "e": ["number"]})
        );
        // Key order is sorted in the serialized form regardless of the
        // `serde_json::Map`'s own iteration order.
        let rendered = serde_json::to_string(&shape).unwrap();
        assert_eq!(
            rendered,
            r#"{"a":"string","b":"number","c":"null","d":"bool","e":["number"]}"#
        );
    }

    #[test]
    fn shape_of_an_empty_array_has_no_element_shape() {
        assert_eq!(shape_of(&json!({"xs": []})), json!({"xs": []}));
    }

    #[test]
    fn shape_of_recurses_into_nested_objects_and_arrays_of_objects() {
        let v = json!({"rows": [{"id": 1, "tag": "x"}]});
        assert_eq!(
            shape_of(&v),
            json!({"rows": [{"id": "number", "tag": "string"}]})
        );
    }

    #[test]
    fn additive_passes_when_every_golden_key_path_still_resolves_at_the_same_type() {
        let golden = shape_of(&json!({"id": 1, "title": "x", "nested": {"a": 1}}));
        let actual = json!({"id": 2, "title": "y", "nested": {"a": 9, "b": "new"}, "extra": true});
        assert!(assert_additive(&golden, &actual, "").is_empty());
    }

    #[test]
    fn additive_fails_on_a_removed_top_level_key() {
        let golden = shape_of(&json!({"id": 1, "title": "x"}));
        let actual = json!({"id": 2});
        let errs = assert_additive(&golden, &actual, "");
        assert_eq!(errs, vec!["title: key removed".to_string()]);
    }

    #[test]
    fn additive_fails_on_a_removed_nested_key() {
        let golden = shape_of(&json!({"verdict": {"state": "approve", "ps": 3}}));
        let actual = json!({"verdict": {"state": "approve"}});
        let errs = assert_additive(&golden, &actual, "");
        assert_eq!(errs, vec!["verdict.ps: key removed".to_string()]);
    }

    #[test]
    fn additive_fails_when_a_scalar_field_changes_type() {
        let golden = shape_of(&json!({"id": 1}));
        let actual = json!({"id": "not-a-number-anymore"});
        let errs = assert_additive(&golden, &actual, "");
        assert_eq!(
            errs,
            vec!["id: golden type \"number\", actual type \"string\"".to_string()]
        );
    }

    #[test]
    fn additive_fails_when_an_object_becomes_a_scalar() {
        // The motivating case this whole unit exists for: README.md §12
        // wraps `base_ref`/`base_source`/… into a nested `base{…}` object
        // on a LATER route (not these — `base_ref` itself is explicitly
        // KEPT, per the design doc) — this proves the check would catch
        // exactly that shape of change if it ever did happen to a pinned
        // scalar.
        let golden = shape_of(&json!({"base_ref": "main"}));
        let actual = json!({"base_ref": {"mode": "tracking", "branch": "main"}});
        let errs = assert_additive(&golden, &actual, "");
        assert_eq!(
            errs,
            vec!["base_ref: golden type \"string\", actual type \"object\"".to_string()]
        );
    }

    #[test]
    fn additive_fails_when_an_array_becomes_an_object() {
        let golden = shape_of(&json!({"warnings": [{"code": "x"}]}));
        let actual = json!({"warnings": {"code": "x"}});
        let errs = assert_additive(&golden, &actual, "");
        assert_eq!(
            errs,
            vec!["warnings: golden is an array, actual is object".to_string()]
        );
    }

    #[test]
    fn additive_never_flags_a_brand_new_key() {
        let golden = shape_of(&json!({"id": 1}));
        let actual = json!({
            "id": 2,
            "base": {"mode": "tracking", "branch": "main"},
            "warnings": [],
            "minted": true,
        });
        assert!(assert_additive(&golden, &actual, "").is_empty());
    }

    #[test]
    fn additive_a_null_golden_leaf_only_requires_presence_of_any_type() {
        let golden = shape_of(&json!({"pr_meta": null}));
        assert!(assert_additive(&golden, &json!({"pr_meta": {"title": "x"}}), "").is_empty());
        assert!(assert_additive(&golden, &json!({"pr_meta": null}), "").is_empty());
        assert!(assert_additive(&golden, &json!({"pr_meta": "surprising"}), "").is_empty());
        let errs = assert_additive(&golden, &json!({}), "");
        assert_eq!(errs, vec!["pr_meta: key removed".to_string()]);
    }

    #[test]
    fn additive_reports_every_problem_not_just_the_first() {
        let golden = shape_of(&json!({"a": 1, "b": "x", "nested": {"c": true}}));
        let actual = json!({"b": 2});
        let errs = sorted(assert_additive(&golden, &actual, ""));
        assert_eq!(
            errs,
            vec![
                "a: key removed".to_string(),
                "b: golden type \"string\", actual type \"number\"".to_string(),
                "nested: key removed".to_string(),
            ]
        );
    }
}
