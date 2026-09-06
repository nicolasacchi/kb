//! A CLOSED YAML-subset front-matter parser (V73-K1).
//!
//! `kbc-review/1` documents open with a `---`-delimited YAML block. This
//! crate has no YAML deserializer (`src/yaml.rs` is a tree-sitter CST
//! outline extractor for the symbol index, not a value parser), and the
//! obvious dependency — `serde_yaml` — is unmaintained upstream. So this
//! module does what this crate already does for every other small fixed
//! grammar it needs (`review_findings::is_valid_finding_slug`'s hand-rolled
//! character class, `entities::zeitwerk`'s read-Ruby-as-TEXT, the
//! `search::grammar` parser): a hand-written parser over a **documented,
//! closed subset**, which REFUSES by name rather than guessing at anything
//! outside it.
//!
//! Refusing is the whole point. A general YAML parser's job is to accept;
//! this one's job is to make sure that what a review document says is what
//! this daemon read. A construct outside the subset produces a
//! [`FrontMatterError`] naming the line and the construct — never a
//! silently-dropped key, and never a value quietly coerced into something
//! adjacent.
//!
//! # The subset
//!
//! **Accepted**
//! - Block mappings, `key: value`, nested by indentation (any consistent
//!   step; two spaces by convention).
//! - Block sequences, `- item`, including `- key: value` items that open a
//!   mapping.
//! - Plain scalars, `'single'`- and `"double"`-quoted scalars (double
//!   quotes honour `\\`, `\"`, `\n`, `\t`, `\r`).
//! - Block scalars: `|`, `|-`, `|+`, `>`, `>-`, `>+`.
//! - One-line flow sequences of scalars: `[]`, `[a, "b", c]`.
//! - `# comment` lines, and blank lines, anywhere.
//! - `true`/`false`/`null`/`~` and integers are typed; everything else
//!   plain is a string.
//!
//! **Refused, by name**
//! - A tab anywhere in indentation (YAML forbids it; silently accepting one
//!   would make two visually identical documents parse differently).
//! - Anchors (`&a`), aliases (`*a`), explicit tags (`!!str`), merge keys
//!   (`<<:`), complex keys (`? `), flow MAPPINGS (`{…}`) and nested flow
//!   sequences.
//! - A duplicate key in one mapping (a real YAML parser silently keeps the
//!   last; here the author gets told).
//! - An indentation level that matches no open block.
//!
//! # Output
//!
//! `serde_json::Value` — so everything downstream is ordinary serde, and
//! the typed [`crate::review_doc::ReviewDoc`] model is built by the same
//! `serde_json::from_value` a JSON body would use.
//!
//! Note that a JSON object does not preserve key order; nothing in
//! `kbc-review/1` depends on front-matter key order (ORDER inside a
//! `reading_order` or `flows` list is a SEQUENCE, which is preserved).

use serde_json::{Map, Value};

/// A refusal. Every variant names the 1-based line within the WHOLE
/// document (not within the front matter), so an editor jump lands right.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontMatterError {
    pub line: u32,
    pub message: String,
}

impl std::fmt::Display for FrontMatterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "front matter line {}: {}", self.line, self.message)
    }
}

impl FrontMatterError {
    fn at(line: u32, message: impl Into<String>) -> Self {
        Self {
            line,
            message: message.into(),
        }
    }
}

/// The split of a `kbc-review/1` document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Split<'a> {
    /// The parsed front-matter mapping (always an object; empty when the
    /// block is empty).
    pub front: Value,
    /// Everything after the closing delimiter, verbatim.
    pub body: &'a str,
    /// 1-based line in the document where `body` starts — so a prose
    /// scanner's line numbers can be translated back to document lines.
    pub body_line: u32,
}

/// Split a document into front matter + body.
///
/// A document that does not open with a `---` line has NO front matter:
/// this returns an empty object and the whole document as body, rather
/// than an error — the tier validation upstream then refuses it by naming
/// the missing required field (`summary_md`), which is a far more useful
/// message than "no front matter".
pub fn split(doc: &str) -> Result<Split<'_>, FrontMatterError> {
    let Some(rest) = strip_open_delimiter(doc) else {
        return Ok(Split {
            front: Value::Object(Map::new()),
            body: doc,
            body_line: 1,
        });
    };
    // Find the closing delimiter: a line that is exactly `---` or `...`.
    let mut consumed = 0usize;
    let mut fm_lines: Vec<(u32, &str)> = Vec::new();
    let mut lineno = 2u32; // line 1 was the opening `---`
    let mut closed = false;
    for line in rest.split_inclusive('\n') {
        let bare = line.strip_suffix('\n').unwrap_or(line);
        let bare = bare.strip_suffix('\r').unwrap_or(bare);
        consumed += line.len();
        if bare == "---" || bare == "..." {
            closed = true;
            lineno += 1;
            break;
        }
        fm_lines.push((lineno, bare));
        lineno += 1;
    }
    if !closed {
        return Err(FrontMatterError::at(
            1,
            "front matter opened with `---` but is never closed by a `---` or `...` line",
        ));
    }
    let body = &rest[consumed..];
    let front = parse_mapping_block(&fm_lines)?;
    Ok(Split {
        front,
        body,
        body_line: lineno,
    })
}

fn strip_open_delimiter(doc: &str) -> Option<&str> {
    for opener in ["---\n", "---\r\n"] {
        if let Some(rest) = doc.strip_prefix(opener) {
            return Some(rest);
        }
    }
    if doc.trim_end() == "---" {
        return Some("");
    }
    None
}

// --- the parser ------------------------------------------------------------

#[derive(Debug, Clone)]
struct Ln {
    no: u32,
    indent: usize,
    text: String,
}

/// Parse a whole front-matter block (a mapping at indent 0).
fn parse_mapping_block(lines: &[(u32, &str)]) -> Result<Value, FrontMatterError> {
    let mut prepared: Vec<Ln> = Vec::new();
    for (no, raw) in lines {
        let indent = measure_indent(raw, *no)?;
        let text = raw[indent..].to_string();
        if text.trim().is_empty() || text.starts_with('#') {
            continue;
        }
        prepared.push(Ln {
            no: *no,
            indent,
            text,
        });
    }
    if prepared.is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    let base = prepared[0].indent;
    let mut i = 0usize;
    let v = parse_block(&prepared, &mut i, base)?;
    if i < prepared.len() {
        return Err(FrontMatterError::at(
            prepared[i].no,
            format!(
                "indentation {} matches no open block (the block above starts at indentation {base})",
                prepared[i].indent
            ),
        ));
    }
    match v {
        Value::Object(_) => Ok(v),
        _ => Err(FrontMatterError::at(
            prepared[0].no,
            "front matter must be a mapping of keys, not a sequence or a bare scalar",
        )),
    }
}

/// Parse the block starting at `lines[*i]`, whose indentation is `indent`.
fn parse_block(lines: &[Ln], i: &mut usize, indent: usize) -> Result<Value, FrontMatterError> {
    if lines[*i].text.starts_with("- ") || lines[*i].text == "-" {
        parse_sequence(lines, i, indent)
    } else {
        parse_mapping(lines, i, indent)
    }
}

fn parse_mapping(lines: &[Ln], i: &mut usize, indent: usize) -> Result<Value, FrontMatterError> {
    let mut map = Map::new();
    while *i < lines.len() {
        let ln = &lines[*i];
        if ln.indent < indent {
            break;
        }
        if ln.indent > indent {
            return Err(FrontMatterError::at(
                ln.no,
                format!("unexpected extra indentation (expected {indent})"),
            ));
        }
        refuse_unsupported(&ln.text, ln.no)?;
        let (key, rest) = split_key(&ln.text, ln.no)?;
        if map.contains_key(&key) {
            return Err(FrontMatterError::at(
                ln.no,
                format!("duplicate key {key:?} in the same mapping"),
            ));
        }
        *i += 1;
        let value = parse_value_for_key(lines, i, indent, rest, ln.no)?;
        map.insert(key, value);
    }
    Ok(Value::Object(map))
}

fn parse_sequence(lines: &[Ln], i: &mut usize, indent: usize) -> Result<Value, FrontMatterError> {
    let mut items = Vec::new();
    while *i < lines.len() {
        let ln = &lines[*i];
        if ln.indent < indent {
            break;
        }
        if ln.indent > indent {
            return Err(FrontMatterError::at(
                ln.no,
                format!("unexpected extra indentation in a sequence (expected {indent})"),
            ));
        }
        let Some(rest) =
            ln.text
                .strip_prefix("- ")
                .or_else(|| if ln.text == "-" { Some("") } else { None })
        else {
            break;
        };
        let rest = rest.trim_end();
        let item_no = ln.no;
        *i += 1;
        if rest.is_empty() {
            // The item's value is the deeper block that follows.
            if *i < lines.len() && lines[*i].indent > indent {
                let child_indent = lines[*i].indent;
                items.push(parse_block(lines, i, child_indent)?);
            } else {
                items.push(Value::Null);
            }
            continue;
        }
        refuse_unsupported(rest, item_no)?;
        // `- key: value` opens a MAPPING whose first key sits at
        // `indent + 2` (the two characters the dash and its space occupy).
        if looks_like_mapping_entry(rest) {
            let child_indent = indent + 2;
            let mut synthetic: Vec<Ln> = vec![Ln {
                no: item_no,
                indent: child_indent,
                text: rest.to_string(),
            }];
            // Pull in every following line that belongs to this item.
            let start = *i;
            let mut j = start;
            while j < lines.len() && lines[j].indent >= child_indent {
                synthetic.push(lines[j].clone());
                j += 1;
            }
            let mut k = 0usize;
            let v = parse_mapping(&synthetic, &mut k, child_indent)?;
            if k < synthetic.len() {
                return Err(FrontMatterError::at(
                    synthetic[k].no,
                    "indentation matches no open block inside this sequence item",
                ));
            }
            *i = j;
            items.push(v);
            continue;
        }
        items.push(parse_scalar(rest, item_no)?);
    }
    Ok(Value::Array(items))
}

/// The value for a `key:` whose remainder is `rest` — either inline, a
/// block scalar, or the deeper block that follows.
fn parse_value_for_key(
    lines: &[Ln],
    i: &mut usize,
    indent: usize,
    rest: &str,
    key_line: u32,
) -> Result<Value, FrontMatterError> {
    let rest = rest.trim();
    if let Some(style) = block_scalar_style(rest) {
        return parse_block_scalar(lines, i, indent, style);
    }
    if !rest.is_empty() {
        return parse_scalar(rest, key_line);
    }
    if *i < lines.len() && lines[*i].indent > indent {
        let child_indent = lines[*i].indent;
        return parse_block(lines, i, child_indent);
    }
    // A sequence may be written at the SAME indentation as its key (very
    // common, and legal YAML):
    //     stops:
    //     - a
    //     - b
    if *i < lines.len()
        && lines[*i].indent == indent
        && (lines[*i].text.starts_with("- ") || lines[*i].text == "-")
    {
        return parse_sequence(lines, i, indent);
    }
    Ok(Value::Null)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockStyle {
    Literal,
    Folded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Chomp {
    Clip,
    Strip,
    Keep,
}

fn block_scalar_style(rest: &str) -> Option<(BlockStyle, Chomp)> {
    let (head, tail) = match rest.chars().next()? {
        '|' => (BlockStyle::Literal, &rest[1..]),
        '>' => (BlockStyle::Folded, &rest[1..]),
        _ => return None,
    };
    let chomp = match tail.trim() {
        "" => Chomp::Clip,
        "-" => Chomp::Strip,
        "+" => Chomp::Keep,
        _ => return None,
    };
    Some((head, chomp))
}

fn parse_block_scalar(
    lines: &[Ln],
    i: &mut usize,
    indent: usize,
    (style, chomp): (BlockStyle, Chomp),
) -> Result<Value, FrontMatterError> {
    let mut raw: Vec<String> = Vec::new();
    let mut child_indent: Option<usize> = None;
    while *i < lines.len() && lines[*i].indent > indent {
        let ln = &lines[*i];
        let ci = *child_indent.get_or_insert(ln.indent);
        if ln.indent < ci {
            break;
        }
        raw.push(format!("{}{}", " ".repeat(ln.indent - ci), ln.text));
        *i += 1;
    }
    let joined = match style {
        BlockStyle::Literal => raw.join("\n"),
        // Folded: a line break between two non-empty lines becomes a
        // space. This subset does not implement the more-indented-line
        // exception (a folded block whose lines are further indented keeps
        // its breaks in real YAML) — `>` in a review document is used for
        // one-paragraph prose, and `parse_block_scalar` REFUSES nothing
        // here because a fold that differs from a real parser's on an
        // exotic input still yields the author's own words, just re-wrapped.
        BlockStyle::Folded => {
            let mut out = String::new();
            for (n, l) in raw.iter().enumerate() {
                if n > 0 {
                    if l.trim().is_empty() || raw[n - 1].trim().is_empty() {
                        out.push('\n');
                    } else {
                        out.push(' ');
                    }
                }
                out.push_str(l);
            }
            out
        }
    };
    let text = match chomp {
        Chomp::Strip => joined.trim_end_matches('\n').to_string(),
        Chomp::Clip => {
            let t = joined.trim_end_matches('\n');
            if t.is_empty() {
                String::new()
            } else {
                format!("{t}\n")
            }
        }
        Chomp::Keep => format!("{joined}\n"),
    };
    Ok(Value::String(text))
}

/// `key: rest` → `(key, rest)`. The key may be quoted; an unquoted key is
/// restricted to a conservative character class so `http://x` is never
/// mistaken for a mapping.
fn split_key(text: &str, no: u32) -> Result<(String, &str), FrontMatterError> {
    if let Some(q) = text.strip_prefix('"').or_else(|| text.strip_prefix('\'')) {
        let quote = text.as_bytes()[0] as char;
        let end = q
            .find(quote)
            .ok_or_else(|| FrontMatterError::at(no, "unterminated quoted key".to_string()))?;
        let key = unquote(&text[..end + 2], no)?;
        let after = text[end + 2..].trim_start();
        let rest = after.strip_prefix(':').ok_or_else(|| {
            FrontMatterError::at(no, "quoted key is not followed by ':'".to_string())
        })?;
        return Ok((key, rest));
    }
    let idx = text.find(':').ok_or_else(|| {
        FrontMatterError::at(
            no,
            format!("expected `key: value`, got {:?}", truncate(text)),
        )
    })?;
    let key = &text[..idx];
    if key.is_empty() {
        return Err(FrontMatterError::at(no, "empty key"));
    }
    if !key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
    {
        return Err(FrontMatterError::at(
            no,
            format!(
                "key {key:?} is outside the accepted key alphabet (alphanumeric, '_', '-', '.')"
            ),
        ));
    }
    let rest = &text[idx + 1..];
    if !rest.is_empty() && !rest.starts_with(' ') {
        return Err(FrontMatterError::at(
            no,
            format!("`{key}:` must be followed by a space or end of line"),
        ));
    }
    Ok((key.to_string(), rest))
}

fn looks_like_mapping_entry(s: &str) -> bool {
    match s.find(':') {
        Some(idx) => {
            let key = &s[..idx];
            let rest = &s[idx + 1..];
            !key.is_empty()
                && key
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
                && (rest.is_empty() || rest.starts_with(' '))
        }
        None => false,
    }
}

fn parse_scalar(s: &str, no: u32) -> Result<Value, FrontMatterError> {
    let s = s.trim();
    if s.starts_with('[') {
        return parse_flow_seq(s, no);
    }
    if s.starts_with('"') || s.starts_with('\'') {
        return Ok(Value::String(unquote(s, no)?));
    }
    Ok(match s {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        "null" | "~" => Value::Null,
        other => match other.parse::<i64>() {
            Ok(n) => Value::Number(n.into()),
            Err(_) => Value::String(other.to_string()),
        },
    })
}

/// `[]` or `[a, "b", c]` — one line, scalars only. A nested `[` or any `{`
/// is refused rather than half-parsed.
fn parse_flow_seq(s: &str, no: u32) -> Result<Value, FrontMatterError> {
    let inner = s
        .strip_prefix('[')
        .and_then(|r| r.strip_suffix(']'))
        .ok_or_else(|| FrontMatterError::at(no, "flow sequence is not closed on the same line"))?;
    if inner.contains('[') || inner.contains('{') {
        return Err(FrontMatterError::at(
            no,
            "nested flow collections are outside the accepted YAML subset — use a block sequence",
        ));
    }
    if inner.trim().is_empty() {
        return Ok(Value::Array(Vec::new()));
    }
    let mut items = Vec::new();
    for part in split_flow_items(inner) {
        items.push(parse_scalar(&part, no)?);
    }
    Ok(Value::Array(items))
}

/// Split on commas that are not inside quotes.
fn split_flow_items(inner: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                cur.push(c);
                if c == '\\' && q == '"' {
                    if let Some(n) = chars.next() {
                        cur.push(n);
                    }
                } else if c == q {
                    quote = None;
                }
            }
            None => match c {
                '"' | '\'' => {
                    quote = Some(c);
                    cur.push(c);
                }
                ',' => {
                    out.push(std::mem::take(&mut cur));
                }
                _ => cur.push(c),
            },
        }
    }
    out.push(cur);
    out.into_iter().map(|s| s.trim().to_string()).collect()
}

fn unquote(s: &str, no: u32) -> Result<String, FrontMatterError> {
    let bytes = s.as_bytes();
    if bytes.len() < 2 {
        return Err(FrontMatterError::at(no, "unterminated quoted scalar"));
    }
    let q = bytes[0] as char;
    if *bytes.last().expect("len >= 2") as char != q {
        return Err(FrontMatterError::at(
            no,
            format!("quoted scalar is not closed with {q:?}"),
        ));
    }
    let inner = &s[1..s.len() - 1];
    if q == '\'' {
        // Single quotes: only `''` is an escape (a literal quote).
        return Ok(inner.replace("''", "'"));
    }
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                return Err(FrontMatterError::at(
                    no,
                    format!(
                        "unsupported escape \\{other} (this subset accepts \\n \\t \\r \\\" \\\\)"
                    ),
                ))
            }
            None => return Err(FrontMatterError::at(no, "trailing backslash")),
        }
    }
    Ok(out)
}

fn measure_indent(raw: &str, no: u32) -> Result<usize, FrontMatterError> {
    let mut n = 0usize;
    for c in raw.chars() {
        match c {
            ' ' => n += 1,
            '\t' => {
                return Err(FrontMatterError::at(
                    no,
                    "tab in indentation — YAML forbids it and two visually identical documents \
                     would parse differently",
                ))
            }
            _ => break,
        }
    }
    Ok(n)
}

/// The constructs this subset refuses outright, checked before anything
/// else so the message names the construct rather than a downstream
/// symptom.
fn refuse_unsupported(text: &str, no: u32) -> Result<(), FrontMatterError> {
    let t = text.trim_start();
    let refusal = if t.starts_with("? ") {
        Some("complex mapping keys (`? `)")
    } else if t.starts_with("<<:") {
        Some("merge keys (`<<:`)")
    } else if t.starts_with('{') {
        Some("flow mappings (`{…}`)")
    } else if t.starts_with('&') {
        Some("anchors (`&name`)")
    } else if t.starts_with('*') {
        Some("aliases (`*name`)")
    } else if t.starts_with('!') {
        Some("explicit tags (`!tag`)")
    } else {
        // A value-position anchor/alias/tag, e.g. `key: &a value`.
        match text.split_once(": ") {
            Some((_, v)) => {
                let v = v.trim_start();
                if v.starts_with('&') {
                    Some("anchors (`&name`)")
                } else if v.starts_with('*') {
                    Some("aliases (`*name`)")
                } else if v.starts_with("!!") || v.starts_with("!<") {
                    Some("explicit tags (`!tag`)")
                } else if v.starts_with('{') {
                    Some("flow mappings (`{…}`)")
                } else {
                    None
                }
            }
            None => None,
        }
    };
    match refusal {
        Some(what) => Err(FrontMatterError::at(
            no,
            format!("{what} are outside the accepted YAML subset for kbc-review/1 front matter"),
        )),
        None => Ok(()),
    }
}

fn truncate(s: &str) -> String {
    s.chars().take(60).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn front(doc: &str) -> Value {
        split(doc).expect("parses").front
    }

    #[test]
    fn a_document_with_no_front_matter_is_all_body() {
        let s = split("# Title\n\nprose\n").expect("ok");
        assert_eq!(s.front, json!({}));
        assert_eq!(s.body, "# Title\n\nprose\n");
        assert_eq!(s.body_line, 1);
    }

    #[test]
    fn body_line_points_at_the_first_line_after_the_closing_delimiter() {
        let s = split("---\na: 1\n---\nbody\n").expect("ok");
        assert_eq!(s.body, "body\n");
        assert_eq!(s.body_line, 4);
    }

    #[test]
    fn scalars_are_typed_conservatively() {
        assert_eq!(
            front("---\na: 1\nb: true\nc: null\nd: ~\ne: hello world\nf: \"1\"\n---\n"),
            json!({"a": 1, "b": true, "c": null, "d": null, "e": "hello world", "f": "1"})
        );
    }

    #[test]
    fn nested_mappings_and_sequences() {
        let doc = "---\n\
                   risk:\n  level: medium\n  why: touches billing\n\
                   reading_order:\n  - chapter: Entry\n    stops:\n      - code:a.rb:1\n      - code:b.rb:2\n\
                   tags: [x, \"y z\", 3]\n\
                   ---\n";
        assert_eq!(
            front(doc),
            json!({
                "risk": {"level": "medium", "why": "touches billing"},
                "reading_order": [
                    {"chapter": "Entry", "stops": ["code:a.rb:1", "code:b.rb:2"]}
                ],
                "tags": ["x", "y z", 3],
            })
        );
    }

    #[test]
    fn a_sequence_may_sit_at_its_keys_own_indentation() {
        assert_eq!(
            front("---\nstops:\n- a\n- b\n---\n"),
            json!({"stops": ["a", "b"]})
        );
    }

    #[test]
    fn block_scalars_literal_folded_and_chomped() {
        let doc = "---\n\
                   lit: |\n  one\n  two\n\
                   strip: |-\n  one\n  two\n\
                   fold: >\n  one\n  two\n\
                   ---\n";
        let v = front(doc);
        assert_eq!(v["lit"], json!("one\ntwo\n"));
        assert_eq!(v["strip"], json!("one\ntwo"));
        assert_eq!(v["fold"], json!("one two\n"));
    }

    #[test]
    fn empty_flow_sequence_is_an_empty_array() {
        assert_eq!(front("---\nomitted: []\n---\n"), json!({"omitted": []}));
    }

    #[test]
    fn double_quoted_escapes_are_honoured_and_unknown_ones_refused() {
        assert_eq!(
            front("---\na: \"line\\nbreak\"\n---\n"),
            json!({"a": "line\nbreak"})
        );
        let e = split("---\na: \"bad \\q\"\n---\n").expect_err("refused");
        assert_eq!(e.line, 2);
        assert!(e.message.contains("unsupported escape"), "{}", e.message);
    }

    #[test]
    fn every_refused_construct_names_itself() {
        for (doc, needle) in [
            ("---\na:\n\tb: 1\n---\n", "tab in indentation"),
            ("---\na: &anchor 1\n---\n", "anchors"),
            ("---\na: *alias\n---\n", "aliases"),
            ("---\na: !!str x\n---\n", "explicit tags"),
            ("---\na: {b: 1}\n---\n", "flow mappings"),
            ("---\n<<: x\n---\n", "merge keys"),
            ("---\na: [[1]]\n---\n", "nested flow collections"),
            ("---\na: 1\na: 2\n---\n", "duplicate key"),
        ] {
            let e = split(doc).expect_err(&format!("{doc:?} must be refused"));
            assert!(
                e.message.contains(needle),
                "{doc:?} refused with {:?}, expected it to name {needle:?}",
                e.message
            );
        }
    }

    #[test]
    fn an_unclosed_front_matter_block_is_refused_not_silently_ignored() {
        let e = split("---\na: 1\nno closing delimiter\n").expect_err("refused");
        assert_eq!(e.line, 1);
        assert!(e.message.contains("never closed"), "{}", e.message);
    }

    #[test]
    fn comments_and_blank_lines_are_ignored_anywhere() {
        assert_eq!(
            front("---\n# top\n\na: 1\n  # nested comment\nb: 2\n---\n"),
            json!({"a": 1, "b": 2})
        );
    }

    #[test]
    fn a_colon_inside_a_plain_value_is_not_a_second_key() {
        assert_eq!(
            front("---\nsummary_md: fixes the race: see the guard\n---\n"),
            json!({"summary_md": "fixes the race: see the guard"})
        );
    }
}
