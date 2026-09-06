//! `haml/1` — the DIVERGENCE-CORPUS projection: the one shape in which
//! this scanner's tree and the real `haml` gem's `Haml::Parser` tree can be
//! compared byte for byte.
//!
//! # Why a projection rather than a direct dump
//!
//! The two models are not the same model, and pretending otherwise would
//! make the corpus test either vacuous or permanently red. This scanner
//! keeps things the gem discards (the sigil that produced a script, every
//! interpolation's byte range, whether a value was reassembled from a `|`
//! continuation); the gem keeps things this scanner has no use for (a
//! compiled `dynamic_attributes` pair, `preserve_tag`). The projection is
//! the INTERSECTION — the facts a reader would call "the structure of this
//! template" — and it is emitted by BOTH sides:
//!
//! * here, from a [`Document`];
//! * by `tests/fixtures/haml/generate_expected.rb`, from the gem, ONCE,
//!   offline, on a developer box (see `CORPUS.md`).
//!
//! # The three deliberate normalisations
//!
//! Each one is a place the two models genuinely disagree in a way that
//! cannot change any edge, symbol or span this daemon derives. They are
//! listed here and in `CORPUS.md`; anything NOT on this list that differs
//! is a real divergence and fails the corpus test.
//!
//! 1. **Ruby and text values are trimmed.** HAML reports `= foo` as the
//!    Ruby `" foo"` (leading space kept) and `%p foo` as the text `"foo"`
//!    (stripped); a `|` join leaves a trailing space. None of that reaches
//!    a tree-sitter parse differently.
//! 2. **An absent inline value and an empty one are the same thing.** The
//!    gem reports `nil` for a tag with children and `""` for a childless
//!    tag with attributes; both mean "no inline content".
//! 3. **Interpolated text is projected as the gem's `script` node.** HAML
//!    compiles `%p a #{b}` into the Ruby string literal `"a #{b}"`, and
//!    `== a`/`& a`/`! a` likewise. [`quote_ruby`] reproduces exactly that
//!    transform, so the projection compares the same node kind on both
//!    sides instead of declaring a permanent structural difference.

use serde_json::{json, Map, Value};

use super::parser::{Document, Inline, NodeKind, Tag, Text};

/// Project `doc` into the corpus shape: `{"nodes": [...]}`.
pub fn project(doc: &Document) -> Value {
    json!({
        "nodes": doc.roots.iter().map(|id| node(doc, *id)).collect::<Vec<_>>(),
    })
}

fn node(doc: &Document, id: usize) -> Value {
    let n = doc.node(id);
    let children: Vec<Value> = n.children.iter().map(|c| node(doc, *c)).collect();
    let mut obj = Map::new();
    match &n.kind {
        NodeKind::Doctype(d) => {
            let (version, ty) = split_doctype(&d.text);
            obj.insert("kind".into(), json!("doctype"));
            obj.insert("version".into(), json!(version));
            obj.insert("doctype".into(), json!(ty));
        }
        NodeKind::Tag(tag) => {
            obj.insert("kind".into(), json!("tag"));
            obj.insert("name".into(), json!(tag.name));
            obj.insert("attributes".into(), attributes(tag));
            obj.insert("self_closing".into(), json!(tag.self_closing));
            obj.insert("inline".into(), inline(tag));
        }
        NodeKind::Plain(t) => {
            // Rule 3: interpolated text IS a script node in HAML's model.
            if t.forced_script || t.has_interpolation() {
                obj.insert("kind".into(), json!("script"));
                obj.insert("ruby".into(), json!(quote_ruby(t.value.trim())));
                obj.insert("keyword".into(), Value::Null);
            } else {
                obj.insert("kind".into(), json!("plain"));
                obj.insert("text".into(), json!(t.value.trim()));
            }
        }
        NodeKind::Script(s) => {
            obj.insert("kind".into(), json!("script"));
            obj.insert("ruby".into(), json!(s.code.trim()));
            obj.insert("keyword".into(), json!(s.keyword));
        }
        NodeKind::SilentScript(s) => {
            obj.insert("kind".into(), json!("silent_script"));
            obj.insert("ruby".into(), json!(s.code.trim()));
            obj.insert("keyword".into(), json!(s.keyword));
        }
        NodeKind::HamlComment(c) => {
            let mut text = c.head.clone();
            if !c.text.is_empty() {
                text.push('\n');
                text.push_str(&c.text);
            }
            obj.insert("kind".into(), json!("haml_comment"));
            obj.insert("text".into(), json!(text.trim()));
        }
        NodeKind::HtmlComment(c) => {
            obj.insert("kind".into(), json!("comment"));
            obj.insert("conditional".into(), json!(c.conditional));
            obj.insert("text".into(), json!(c.body.value.trim()));
        }
        NodeKind::Filter(f) => {
            obj.insert("kind".into(), json!("filter"));
            obj.insert("name".into(), json!(f.name));
            obj.insert("text".into(), json!(f.text.trim_end()));
        }
    }
    obj.insert("children".into(), Value::Array(children));
    Value::Object(obj)
}

/// The static `class`/`id`/HTML-style pairs, as a SORTED object — the two
/// sides' natural orders differ (HAML merges into a Ruby hash, this
/// scanner keeps source order) and the ordering carries no meaning.
fn attributes(tag: &Tag) -> Value {
    let mut map = Map::new();
    for (k, v) in tag.static_attributes() {
        map.insert(k, json!(v));
    }
    let mut keys: Vec<String> = map.keys().cloned().collect();
    keys.sort();
    let mut sorted = Map::new();
    for k in keys {
        let v = map.get(&k).cloned().unwrap_or(Value::Null);
        sorted.insert(k, v);
    }
    Value::Object(sorted)
}

/// Rule 2 + rule 3: `null`, `{"text": …}` or `{"script": …}`.
fn inline(tag: &Tag) -> Value {
    match &tag.inline {
        None => Value::Null,
        Some(Inline::Script(s)) => {
            let code = s.code.trim();
            if code.is_empty() {
                Value::Null
            } else {
                json!({ "script": code })
            }
        }
        Some(Inline::Text(t)) => inline_text(t),
    }
}

fn inline_text(t: &Text) -> Value {
    let value = t.value.trim();
    if value.is_empty() {
        return Value::Null;
    }
    if t.forced_script || t.has_interpolation() {
        json!({ "script": quote_ruby(value) })
    } else {
        json!({ "text": value })
    }
}

/// HAML's own interpolated-text→Ruby transform: wrap in double quotes,
/// escaping backslashes and double quotes and NOTHING else — `#{…}` is
/// deliberately left intact, because reproducing it is the entire point.
pub fn quote_ruby(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `!!! 5` → `(Some("5"), "")`, `!!! XML` → `(None, "xml")`, `!!!` →
/// `(None, "")` — HAML's own split: a numeric first word is the VERSION,
/// anything else is the doctype TYPE (lowercased).
pub fn split_doctype(text: &str) -> (Option<String>, String) {
    let mut words = text.split_whitespace();
    match words.next() {
        None => (None, String::new()),
        Some(w) if w.chars().all(|c| c.is_ascii_digit() || c == '.') => {
            (Some(w.to_string()), String::new())
        }
        Some(w) => (None, w.to_ascii_lowercase()),
    }
}
