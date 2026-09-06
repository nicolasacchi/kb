//! v0.10 Q1 — structured query DSL.
//!
//! Grammar (Pratt-friendly, but kept hand-written for tight error
//! messages):
//!
//! ```text
//! Expr  := Conj ('OR' Conj)*
//! Conj  := Term ('AND' Term | WS Term)*       // implicit AND between
//!                                              // adjacent terms is allowed
//! Term  := 'NOT' Atom | Atom | '(' Expr ')'
//! Atom  := Key ':' Value
//! Key   := tag | folder | caps | cap | since | index | scope | text
//! Value := bareword | quoted_string
//! ```
//!
//! `to_docs_query` lowers the boolean `Expr` to **disjunctive normal
//! form** — an OR of AND-conjuncts — and packs it into a flat
//! [`DocsQuery`]: the first conjunct fills the struct's own filter
//! fields, the rest become `DocsQuery::alternatives`, and `NOT`-atoms
//! populate the `exclude_*` fields. `docs_query::matches` evaluates the
//! whole thing in-memory over the gallery row-set (a row matches iff any
//! conjunct matches), so OR/NOT need no SQL change. The cartesian
//! expansion is bounded by [`MAX_DNF_CONJUNCTS`]; predicates `matches`
//! can't express in-memory (`NOT since/text/scope`) stay as warnings in
//! `Conversion::warnings` rather than silently misfiltering.
//!
//! Backwards compat: [`DocsQuery::to_expr`] turns existing URL params
//! (`?tags=…&folder=…&caps=…&since=…&index=1`) into an `Expr`, so the
//! QueryRibbon can render token chips for legacy URLs without a
//! migration. [`Expr::to_url_params`] is the inverse — emit the same
//! params from an AST so saved queries (Q4) stay parameter-compatible.

use crate::docs_query::{Capability, DocsQuery};
use std::fmt;

/// One atomic predicate `key:value` (or `NOT key:value`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Atom {
    pub key: Key,
    pub value: String,
    pub negated: bool,
}

/// The DSL's keys. Unknown keys parse as `Key::Text` so the user's
/// raw text isn't lost; the SPA renders them with a warning chip.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Key {
    Tag,
    Folder,
    Cap,
    Since,
    Index,
    Scope,
    Text,
    /// Unrecognised — kept verbatim so the round-trip is lossless.
    Other(String),
}

impl Key {
    /// Parse the bareword key. Unknown keys produce `Key::Other` so
    /// `tag:` round-trips losslessly. (Named `from_lex` rather than
    /// `from_str` to sidestep clippy's `should_implement_trait` lint —
    /// implementing `FromStr` here would require a never-fail `Err`
    /// type the lossless-Other contract doesn't actually need.)
    pub fn from_lex(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "tag" => Key::Tag,
            "folder" => Key::Folder,
            // `caps` and `cap` are aliased so both `caps:table` (the
            // sidebar label) and `cap:table` (singular) parse the same.
            "cap" | "caps" => Key::Cap,
            "since" => Key::Since,
            "index" => Key::Index,
            "scope" => Key::Scope,
            "text" | "q" => Key::Text,
            other => Key::Other(other.to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Key::Tag => "tag",
            Key::Folder => "folder",
            Key::Cap => "cap",
            Key::Since => "since",
            Key::Index => "index",
            Key::Scope => "scope",
            Key::Text => "text",
            Key::Other(s) => s.as_str(),
        }
    }
}

/// Abstract syntax. AND/OR are sequence types so a 3-arg AND parses
/// as one node (not a binary tree). Atoms carry their own `negated`
/// bit so `NOT tag:x` is one node, not two.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    Atom(Atom),
    And(Vec<Expr>),
    Or(Vec<Expr>),
    Empty,
}

impl Expr {
    /// Constructor for an AND that flattens nested ANDs + trims Empty
    /// children. A single-element AND collapses to its child.
    pub fn and(parts: impl IntoIterator<Item = Expr>) -> Self {
        let mut flat: Vec<Expr> = Vec::new();
        for p in parts {
            match p {
                Expr::Empty => {}
                Expr::And(inner) => flat.extend(inner),
                other => flat.push(other),
            }
        }
        match flat.len() {
            0 => Expr::Empty,
            1 => flat.into_iter().next().unwrap(),
            _ => Expr::And(flat),
        }
    }

    /// Constructor for an OR that flattens + trims Empty / single-child.
    pub fn or(parts: impl IntoIterator<Item = Expr>) -> Self {
        let mut flat: Vec<Expr> = Vec::new();
        for p in parts {
            match p {
                Expr::Empty => {}
                Expr::Or(inner) => flat.extend(inner),
                other => flat.push(other),
            }
        }
        match flat.len() {
            0 => Expr::Empty,
            1 => flat.into_iter().next().unwrap(),
            _ => Expr::Or(flat),
        }
    }
}

/// `to_docs_query` outcome — the `Expr` lowered to a flat [`DocsQuery`]
/// (first conjunct in the struct's fields, the rest in `alternatives`,
/// `NOT`-atoms in `exclude_*`). `text` carries the free-text terms
/// (a single BM25 string, collected across the whole expression rather
/// than per conjunct). `warnings` carry constructs `docs_query::matches`
/// can't express — unknown keys/caps, `NOT since/text/scope`, and an
/// over-cap OR expansion; the SPA renders them as a small warning chip
/// in the ribbon.
#[derive(Debug, Clone, Default)]
pub struct Conversion {
    pub query: DocsQuery,
    pub text: Option<String>,
    pub warnings: Vec<String>,
}

impl Expr {
    /// Lower to the flat [`DocsQuery`] the docs route filters with.
    /// Expands the boolean expression to disjunctive normal form (an OR
    /// of AND-conjuncts), turning each conjunct into a `DocsQuery` —
    /// positive atoms fill the filter fields, negated atoms fill
    /// `exclude_*`. The first conjunct is the base; the rest land in
    /// `alternatives`. `docs_query::matches` then matches a row iff any
    /// conjunct matches. Free-text terms are collected separately into
    /// `text` (a single BM25 string, not a per-conjunct predicate).
    pub fn to_docs_query(&self) -> Conversion {
        let mut out = Conversion::default();

        // Free text isn't a per-row predicate `matches` evaluates — pull
        // it from the whole expression (deduped, document order) so
        // `a AND (tag:x OR tag:y)` doesn't double its text across the
        // two DNF conjuncts.
        collect_text(self, &mut out.text, &mut out.warnings);

        // `MAX_DNF_CONJUNCTS + 1` is the build ceiling so an overflow is
        // distinguishable from an exactly-full (legitimate) expansion.
        let mut conjuncts = dnf_build(self, MAX_DNF_CONJUNCTS + 1);
        if conjuncts.len() > MAX_DNF_CONJUNCTS {
            conjuncts.truncate(MAX_DNF_CONJUNCTS);
            out.warnings.push(format!(
                "OR expansion capped at {MAX_DNF_CONJUNCTS} alternatives; refine the query"
            ));
        }

        let mut queries: Vec<DocsQuery> = Vec::with_capacity(conjuncts.len());
        for conj in conjuncts {
            let mut q = DocsQuery::default();
            for atom in conj {
                apply_atom(&mut q, &mut out.warnings, atom);
            }
            queries.push(q);
        }
        // `dnf_build` always yields ≥1 conjunct (`Empty` → one empty
        // conjunct = match-all), so `next()` is `Some`; default-guard anyway.
        let mut it = queries.into_iter();
        out.query = it.next().unwrap_or_default();
        out.query.alternatives = it.collect();
        dedup_warnings(&mut out.warnings);
        out
    }

    /// Inverse: build a canonical expression from an existing
    /// `DocsQuery` so the QueryRibbon can render token chips for
    /// URLs that pre-date `?q=`.
    pub fn from_docs_query(q: &DocsQuery, text: Option<&str>) -> Self {
        let mut parts: Vec<Expr> = Vec::new();
        for t in &q.tags {
            parts.push(Expr::Atom(Atom {
                key: Key::Tag,
                value: t.clone(),
                negated: false,
            }));
        }
        if let Some(f) = &q.folder {
            parts.push(Expr::Atom(Atom {
                key: Key::Folder,
                value: f.clone(),
                negated: false,
            }));
        }
        for c in &q.caps {
            parts.push(Expr::Atom(Atom {
                key: Key::Cap,
                value: cap_to_str(*c).to_string(),
                negated: false,
            }));
        }
        if let Some(since) = q.since_unix {
            // Convert the unix timestamp back to a relative tag where
            // we can (the SPA only writes 7d/30d to the URL today), else
            // emit the literal seconds — the canonical form is round-
            // trippable via `to_docs_query`.
            parts.push(Expr::Atom(Atom {
                key: Key::Since,
                value: format!("{}", since),
                negated: false,
            }));
        }
        if q.index_only {
            parts.push(Expr::Atom(Atom {
                key: Key::Index,
                value: "true".to_string(),
                negated: false,
            }));
        }
        if let Some(t) = text.filter(|s| !s.trim().is_empty()) {
            parts.push(Expr::Atom(Atom {
                key: Key::Text,
                value: t.to_string(),
                negated: false,
            }));
        }
        Expr::and(parts)
    }
}

fn cap_to_str(c: Capability) -> &'static str {
    match c {
        Capability::Svg => "svg",
        Capability::Interactive => "interactive",
        Capability::Code => "code",
        Capability::Longread => "longread",
    }
}

fn parse_cap(s: &str) -> Option<Capability> {
    match s.to_ascii_lowercase().as_str() {
        "svg" | "visual" => Some(Capability::Svg),
        "interactive" => Some(Capability::Interactive),
        "code" => Some(Capability::Code),
        "longread" | "long-read" => Some(Capability::Longread),
        _ => None,
    }
}

fn parse_since(value: &str) -> Result<i64, String> {
    // "7d" / "30d" / "all" / a raw unix timestamp ("1700000000"). The
    // SPA writes the relative forms; the canonical AST round-trip
    // serialises a raw integer for fidelity. We avoid a second colon
    // in the value (e.g. "unix:N") because the tokenizer would split
    // on it — `since:1700000000` is the round-trip form instead.
    if value == "all" {
        return Err("since:all is the absence of a filter — drop the atom".into());
    }
    let now = chrono::Utc::now().timestamp();
    if let Some(rest) = value.strip_suffix('d') {
        let days: i64 = rest.parse().map_err(|e| format!("since {value}: {e}"))?;
        return Ok(now - days.saturating_mul(86_400));
    }
    if let Some(rest) = value.strip_suffix('h') {
        let hours: i64 = rest.parse().map_err(|e| format!("since {value}: {e}"))?;
        return Ok(now - hours.saturating_mul(3_600));
    }
    if let Ok(unix) = value.parse::<i64>() {
        return Ok(unix);
    }
    Err(format!("since: unsupported unit in {value:?}"))
}

/// Upper bound on the number of conjuncts after DNF expansion. A query
/// is a cartesian product across its AND-ed OR-groups, so `(a OR b) AND
/// (c OR d) AND …` grows multiplicatively; cap it so a request-supplied
/// query can't blow up memory. Realistic queries have a single OR group
/// and stay far under this. On overflow the lowering truncates and warns.
const MAX_DNF_CONJUNCTS: usize = 64;

/// Lower an `Expr` to disjunctive normal form: a list of conjuncts, each
/// a list of (possibly negated) literal atoms. A row matches the query
/// iff it matches ANY conjunct. `limit` caps the running conjunct count
/// during construction (so the cartesian product in the `And` arm can't
/// allocate unboundedly); the caller passes `MAX_DNF_CONJUNCTS + 1` and
/// detects truncation by the returned length.
fn dnf_build<'a>(e: &'a Expr, limit: usize) -> Vec<Vec<&'a Atom>> {
    match e {
        // One empty conjunct = match-all (an empty query shows everything).
        Expr::Empty => vec![Vec::new()],
        Expr::Atom(a) => vec![vec![a]],
        Expr::Or(parts) => {
            let mut acc: Vec<Vec<&'a Atom>> = Vec::new();
            for p in parts {
                for conj in dnf_build(p, limit) {
                    acc.push(conj);
                    if acc.len() >= limit {
                        return acc;
                    }
                }
            }
            acc
        }
        Expr::And(parts) => {
            // Cartesian product of the children's DNFs: pick one conjunct
            // from each child and concatenate.
            let mut acc: Vec<Vec<&'a Atom>> = vec![Vec::new()];
            for p in parts {
                let child = dnf_build(p, limit);
                let mut next: Vec<Vec<&'a Atom>> = Vec::new();
                let mut hit_limit = false;
                'pairs: for base in &acc {
                    for add in &child {
                        let mut merged = base.clone();
                        merged.extend(add.iter().copied());
                        next.push(merged);
                        if next.len() >= limit {
                            hit_limit = true;
                            break 'pairs;
                        }
                    }
                }
                acc = next;
                if hit_limit {
                    break;
                }
            }
            acc
        }
    }
}

/// Fold a single literal atom into one conjunct's `DocsQuery`. Positive
/// atoms fill the filter fields; negated atoms fill `exclude_*` for the
/// predicates `docs_query::matches` can evaluate (tag/folder/cap/index).
/// `NOT since/text/scope` aren't in-memory predicates, so they warn
/// rather than silently misfilter. `Text` is handled by [`collect_text`]
/// (top-level), not here.
fn apply_atom(q: &mut DocsQuery, warnings: &mut Vec<String>, a: &Atom) {
    match (&a.key, a.negated) {
        (Key::Tag, false) => q.tags.push(a.value.clone()),
        (Key::Tag, true) => q.exclude_tags.push(a.value.clone()),
        (Key::Folder, false) => q.folder = Some(a.value.clone()),
        (Key::Folder, true) => q.exclude_folders.push(a.value.clone()),
        (Key::Cap, negated) => match parse_cap(&a.value) {
            Some(c) if !negated => q.caps.push(c),
            Some(c) => q.exclude_caps.push(c),
            None => warnings.push(format!("unknown capability: {}", a.value)),
        },
        (Key::Since, false) => match parse_since(&a.value) {
            Ok(t) => q.since_unix = Some(t),
            Err(msg) => warnings.push(msg),
        },
        (Key::Since, true) => warnings.push(format!(
            "NOT since:{} is unsupported (no upper-bound/before filter)",
            a.value
        )),
        (Key::Index, false) => {
            // `index:true` / `index:1` / `index:` (presence) all opt into
            // index-only.
            q.index_only = matches!(
                a.value.to_ascii_lowercase().as_str(),
                "true" | "1" | "yes" | ""
            );
        }
        (Key::Index, true) => {
            // `NOT index:true` → exclude root index pages. `NOT
            // index:false` has nothing to exclude (a no-op).
            if matches!(
                a.value.to_ascii_lowercase().as_str(),
                "true" | "1" | "yes" | ""
            ) {
                q.exclude_index = true;
            }
        }
        (Key::Scope, _) => {
            // scope:* belongs to /memory and isn't expressed in DocsQuery
            // — the route handler reads it from the URL directly.
        }
        // Text is collected top-level by `collect_text`; ignore here.
        (Key::Text, _) => {}
        (Key::Other(name), _) => warnings.push(format!(
            "unknown key {name:?}; valid keys: tag, folder, cap(s), since, index, scope, text"
        )),
    }
}

/// Gather the free-text terms from the whole expression into a single
/// space-joined string (document order). Text is a BM25 query, not a
/// per-row predicate, so it's collected once across all branches rather
/// than per DNF conjunct (which would duplicate it). Negated text isn't
/// expressible, so it warns.
fn collect_text(e: &Expr, text: &mut Option<String>, warnings: &mut Vec<String>) {
    match e {
        Expr::Empty => {}
        Expr::Atom(a) if a.key == Key::Text => {
            if a.negated {
                warnings.push(format!("NOT text:{:?} is unsupported", a.value));
            } else {
                *text = Some(match text.take() {
                    Some(prev) => format!("{prev} {}", a.value),
                    None => a.value.clone(),
                });
            }
        }
        Expr::Atom(_) => {}
        Expr::And(parts) | Expr::Or(parts) => {
            for p in parts {
                collect_text(p, text, warnings);
            }
        }
    }
}

/// Drop exact-duplicate warnings while preserving first-seen order. DNF
/// expansion can repeat a shared atom (e.g. an unknown key AND-ed with an
/// OR group lands in every conjunct), which would otherwise surface the
/// same warning N times.
fn dedup_warnings(warnings: &mut Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    warnings.retain(|w| seen.insert(w.clone()));
}

// === Display (canonical form) ==============================================

impl fmt::Display for Atom {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.negated {
            f.write_str("NOT ")?;
        }
        write!(f, "{}:{}", self.key.as_str(), maybe_quote(&self.value))
    }
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Expr::Empty => Ok(()),
            Expr::Atom(a) => write!(f, "{a}"),
            Expr::And(parts) => {
                for (i, p) in parts.iter().enumerate() {
                    if i > 0 {
                        f.write_str(" AND ")?;
                    }
                    fmt_paren(f, p, |x| matches!(x, Expr::Or(_)))?;
                }
                Ok(())
            }
            Expr::Or(parts) => {
                for (i, p) in parts.iter().enumerate() {
                    if i > 0 {
                        f.write_str(" OR ")?;
                    }
                    fmt_paren(f, p, |_| false)?;
                }
                Ok(())
            }
        }
    }
}

fn fmt_paren(
    f: &mut fmt::Formatter<'_>,
    e: &Expr,
    needs_paren: impl Fn(&Expr) -> bool,
) -> fmt::Result {
    if needs_paren(e) {
        f.write_str("(")?;
        write!(f, "{e}")?;
        f.write_str(")")
    } else {
        write!(f, "{e}")
    }
}

fn maybe_quote(s: &str) -> String {
    if s.is_empty()
        || s.bytes()
            .any(|b| matches!(b, b' ' | b'\t' | b'(' | b')' | b'"'))
    {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        s.to_string()
    }
}

// === Parser ================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    UnexpectedEof,
    UnexpectedToken {
        found: String,
        ctx: &'static str,
    },
    UnterminatedString,
    /// The expression nests parens / `NOT` past [`MAX_PARSE_DEPTH`]. A
    /// bound on the recursive-descent depth so a hostile `((((…))))` can't
    /// blow the stack (the query string is request-supplied).
    TooDeep,
}

/// Max recursion depth for the recursive-descent parser — each `(` group
/// and each `NOT` descends one level. 64 is far beyond any human-written
/// query but well under the stack budget.
const MAX_PARSE_DEPTH: usize = 64;

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::UnexpectedEof => f.write_str("unexpected end of input"),
            ParseError::UnexpectedToken { found, ctx } => {
                write!(f, "unexpected {found:?} ({ctx})")
            }
            ParseError::UnterminatedString => f.write_str("unterminated quoted string"),
            ParseError::TooDeep => {
                write!(f, "query nesting too deep (max {MAX_PARSE_DEPTH} levels)")
            }
        }
    }
}

impl std::error::Error for ParseError {}

/// Parse a query string. Whitespace + an empty string both yield
/// `Expr::Empty` (no filter — show everything).
pub fn parse(input: &str) -> Result<Expr, ParseError> {
    let mut tokens = tokenize(input)?;
    if tokens.is_empty() {
        return Ok(Expr::Empty);
    }
    let expr = parse_or(&mut tokens, 0)?;
    if !tokens.is_empty() {
        return Err(ParseError::UnexpectedToken {
            found: format!("{:?}", tokens.remove(0)),
            ctx: "trailing input after expression",
        });
    }
    Ok(expr)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Word(String),
    Quoted(String),
    Colon,
    LParen,
    RParen,
    AndKw,
    OrKw,
    NotKw,
}

fn tokenize(input: &str) -> Result<Vec<Token>, ParseError> {
    let mut out = Vec::new();
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        match b {
            b'(' => {
                out.push(Token::LParen);
                i += 1;
            }
            b')' => {
                out.push(Token::RParen);
                i += 1;
            }
            b':' => {
                out.push(Token::Colon);
                i += 1;
            }
            b'"' => {
                // Slice by byte offsets (same as the bareword path). Delimiters
                // `"` / `\` are ASCII so span ends always land on char
                // boundaries; `u8 as char` would mojibake multi-byte UTF-8.
                i += 1;
                let mut s = String::new();
                let mut closed = false;
                let mut start = i;
                while i < bytes.len() {
                    if bytes[i] == b'\\' && i + 1 < bytes.len() {
                        s.push_str(&input[start..i]);
                        // Escaped unit = one full UTF-8 char after the `\`.
                        let esc = input[i + 1..].chars().next().expect("byte exists");
                        s.push(esc);
                        i += 1 + esc.len_utf8();
                        start = i;
                    } else if bytes[i] == b'"' {
                        s.push_str(&input[start..i]);
                        i += 1;
                        closed = true;
                        break;
                    } else {
                        i += 1;
                    }
                }
                if !closed {
                    return Err(ParseError::UnterminatedString);
                }
                out.push(Token::Quoted(s));
            }
            _ => {
                let start = i;
                while i < bytes.len()
                    && !bytes[i].is_ascii_whitespace()
                    && bytes[i] != b':'
                    && bytes[i] != b'('
                    && bytes[i] != b')'
                {
                    i += 1;
                }
                let word: &str = &input[start..i];
                let tok = match word {
                    "AND" | "and" | "&&" => Token::AndKw,
                    "OR" | "or" | "||" => Token::OrKw,
                    "NOT" | "not" | "!" => Token::NotKw,
                    _ => Token::Word(word.to_string()),
                };
                out.push(tok);
            }
        }
    }
    Ok(out)
}

fn parse_or(tokens: &mut Vec<Token>, depth: usize) -> Result<Expr, ParseError> {
    let mut parts = vec![parse_and(tokens, depth)?];
    while let Some(Token::OrKw) = tokens.first() {
        tokens.remove(0);
        parts.push(parse_and(tokens, depth)?);
    }
    Ok(Expr::or(parts))
}

fn parse_and(tokens: &mut Vec<Token>, depth: usize) -> Result<Expr, ParseError> {
    let mut parts = vec![parse_term(tokens, depth)?];
    loop {
        match tokens.first() {
            Some(Token::AndKw) => {
                tokens.remove(0);
                parts.push(parse_term(tokens, depth)?);
            }
            // Implicit AND between adjacent terms: `tag:a folder:b` parses
            // the same as `tag:a AND folder:b`.
            Some(Token::Word(_))
            | Some(Token::Quoted(_))
            | Some(Token::LParen)
            | Some(Token::NotKw) => {
                parts.push(parse_term(tokens, depth)?);
            }
            _ => break,
        }
    }
    Ok(Expr::and(parts))
}

fn parse_term(tokens: &mut Vec<Token>, depth: usize) -> Result<Expr, ParseError> {
    // Each `(` group / `NOT` descends one recursion level; bound it so a
    // request-supplied `((((…))))` or `NOT NOT NOT …` can't overflow the
    // stack.
    if depth > MAX_PARSE_DEPTH {
        return Err(ParseError::TooDeep);
    }
    match tokens.first() {
        Some(Token::NotKw) => {
            tokens.remove(0);
            let inner = parse_term(tokens, depth + 1)?;
            Ok(negate(inner))
        }
        Some(Token::LParen) => {
            tokens.remove(0);
            let inner = parse_or(tokens, depth + 1)?;
            match tokens.first() {
                Some(Token::RParen) => {
                    tokens.remove(0);
                    Ok(inner)
                }
                Some(other) => Err(ParseError::UnexpectedToken {
                    found: format!("{other:?}"),
                    ctx: "expected ')' to close group",
                }),
                None => Err(ParseError::UnexpectedEof),
            }
        }
        Some(_) => parse_atom(tokens),
        None => Err(ParseError::UnexpectedEof),
    }
}

fn parse_atom(tokens: &mut Vec<Token>) -> Result<Expr, ParseError> {
    let key_word = match tokens.first().cloned() {
        Some(Token::Word(w)) => w,
        Some(Token::Quoted(w)) => w,
        Some(other) => {
            return Err(ParseError::UnexpectedToken {
                found: format!("{other:?}"),
                ctx: "expected a key",
            })
        }
        None => return Err(ParseError::UnexpectedEof),
    };
    tokens.remove(0);
    let key = Key::from_lex(&key_word);
    // No colon — treat the whole word as free text. Lets users type
    // `rust patterns` and have it match a `text` query.
    match tokens.first() {
        Some(Token::Colon) => {
            tokens.remove(0);
        }
        _ => {
            return Ok(Expr::Atom(Atom {
                key: Key::Text,
                value: key_word,
                negated: false,
            }));
        }
    }
    let value = match tokens.first().cloned() {
        Some(Token::Word(w)) => {
            tokens.remove(0);
            w
        }
        Some(Token::Quoted(w)) => {
            tokens.remove(0);
            w
        }
        Some(other) => {
            return Err(ParseError::UnexpectedToken {
                found: format!("{other:?}"),
                ctx: "expected a value after ':'",
            });
        }
        None => return Err(ParseError::UnexpectedEof),
    };
    Ok(Expr::Atom(Atom {
        key,
        value,
        negated: false,
    }))
}

fn negate(e: Expr) -> Expr {
    match e {
        Expr::Atom(mut a) => {
            a.negated = !a.negated;
            Expr::Atom(a)
        }
        // De Morgan: NOT (a AND b) = (NOT a) OR (NOT b); NOT (a OR b) =
        // (NOT a) AND (NOT b). Flip the connective so a negated group
        // lowers correctly once `NOT` actually filters (the `or`/`and`
        // constructors re-flatten the result).
        Expr::And(parts) => Expr::or(parts.into_iter().map(negate)),
        Expr::Or(parts) => Expr::and(parts.into_iter().map(negate)),
        Expr::Empty => Expr::Empty,
    }
}

// === URL-param interop =====================================================

impl Expr {
    /// Emit the URL-param form (`?tags=…&folder=…&caps=…&since=…&index=1`)
    /// the existing gallery + docs routes understand. Drops anything
    /// that can't round-trip (OR / NOT / scope / text).
    pub fn to_url_params(&self) -> Vec<(&'static str, String)> {
        let conv = self.to_docs_query();
        let mut out: Vec<(&'static str, String)> = Vec::new();
        if !conv.query.tags.is_empty() {
            out.push(("tags", conv.query.tags.join(",")));
        }
        if let Some(f) = &conv.query.folder {
            out.push(("folder", f.clone()));
        }
        if !conv.query.caps.is_empty() {
            let s = conv
                .query
                .caps
                .iter()
                .map(|c| cap_to_str(*c).to_string())
                .collect::<Vec<_>>()
                .join(",");
            out.push(("caps", s));
        }
        if let Some(_t) = conv.query.since_unix {
            // The route only accepts the relative forms (7d/30d), not
            // unix timestamps. Emit "since=all" as a no-op; the caller
            // can replace with a relative when the source had one.
            // Leave the key out entirely if we don't have a clean form;
            // the SPA owns "absent = no filter".
        }
        if conv.query.index_only {
            out.push(("index", "1".to_string()));
        }
        if let Some(t) = conv.text {
            out.push(("q", t));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Expr {
        parse(s).unwrap_or_else(|e| panic!("parse {s:?}: {e}"))
    }

    #[test]
    fn empty_input_parses_as_empty() {
        assert_eq!(p(""), Expr::Empty);
        assert_eq!(p("   \t\n"), Expr::Empty);
    }

    #[test]
    fn single_atom_parses() {
        let e = p("tag:research");
        assert_eq!(
            e,
            Expr::Atom(Atom {
                key: Key::Tag,
                value: "research".into(),
                negated: false,
            })
        );
    }

    #[test]
    fn implicit_and_and_explicit_and_agree() {
        // `tag:a tag:b` and `tag:a AND tag:b` build the same AST.
        let a = p("tag:a tag:b");
        let b = p("tag:a AND tag:b");
        assert_eq!(a, b);
        match a {
            Expr::And(parts) => assert_eq!(parts.len(), 2),
            other => panic!("expected And, got {other:?}"),
        }
    }

    #[test]
    fn or_parses_and_displays_with_parens_inside_and() {
        // `(tag:a OR tag:b) AND folder:foo` round-trips.
        let e = p("(tag:a OR tag:b) AND folder:foo");
        let s = e.to_string();
        assert_eq!(s, "(tag:a OR tag:b) AND folder:foo");
        let e2 = p(&s);
        assert_eq!(e, e2, "round-trip parse failed: {s}");
    }

    #[test]
    fn not_negates_the_atom() {
        let e = p("NOT folder:_template");
        assert_eq!(
            e,
            Expr::Atom(Atom {
                key: Key::Folder,
                value: "_template".into(),
                negated: true,
            })
        );
    }

    #[test]
    fn quoted_values_carry_spaces() {
        let e = p(r#"text:"agent native web""#);
        assert_eq!(
            e,
            Expr::Atom(Atom {
                key: Key::Text,
                value: "agent native web".into(),
                negated: false,
            })
        );
        // Display re-quotes since the value has spaces.
        assert_eq!(e.to_string(), "text:\"agent native web\"");
    }

    #[test]
    fn quoted_values_preserve_multibyte_utf8() {
        // Accented Latin — each non-ASCII char is 2 bytes; the old
        // `bytes[i] as char` path would mojibake "città".
        let e = p(r#"text:"città""#);
        assert_eq!(
            e,
            Expr::Atom(Atom {
                key: Key::Text,
                value: "città".into(),
                negated: false,
            })
        );
        // CJK + spaces inside quotes.
        let e = p(r#"text:"日本語 クエリ""#);
        assert_eq!(
            e,
            Expr::Atom(Atom {
                key: Key::Text,
                value: "日本語 クエリ".into(),
                negated: false,
            })
        );
        // Escaped quote inside a CJK string — `\"` must not split a char.
        let e = p(r#"text:"日本語\"クエリ""#);
        assert_eq!(
            e,
            Expr::Atom(Atom {
                key: Key::Text,
                value: "日本語\"クエリ".into(),
                negated: false,
            })
        );
    }

    #[test]
    fn bareword_without_colon_becomes_text() {
        let e = p("research");
        assert_eq!(
            e,
            Expr::Atom(Atom {
                key: Key::Text,
                value: "research".into(),
                negated: false,
            })
        );
    }

    #[test]
    fn unknown_key_round_trips_as_other() {
        let e = p("severity:high");
        if let Expr::Atom(a) = &e {
            assert_eq!(a.key, Key::Other("severity".into()));
        } else {
            panic!("expected Atom");
        }
        // to_docs_query surfaces a warning but doesn't drop the atom.
        let conv = e.to_docs_query();
        assert!(
            !conv.warnings.is_empty(),
            "expected warning for unknown key"
        );
        assert!(conv.warnings[0].contains("unknown key"));
    }

    #[test]
    fn unknown_capability_warns() {
        let e = p("cap:rocket");
        let conv = e.to_docs_query();
        assert!(conv
            .warnings
            .iter()
            .any(|w| w.contains("unknown capability")));
        // No capability landed on the query.
        assert!(conv.query.caps.is_empty());
    }

    #[test]
    fn missing_value_is_an_error() {
        let r = parse("tag:");
        assert!(r.is_err(), "expected error, got {r:?}");
    }

    #[test]
    fn unterminated_string_is_an_error() {
        assert!(matches!(
            parse(r#"text:"a"#),
            Err(ParseError::UnterminatedString)
        ));
    }

    #[test]
    fn to_docs_query_collects_tags_and_caps() {
        let e = p("tag:a tag:b cap:code folder:foo index:true");
        let conv = e.to_docs_query();
        assert_eq!(conv.query.tags, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(conv.query.caps, vec![Capability::Code]);
        assert_eq!(conv.query.folder.as_deref(), Some("foo"));
        assert!(conv.query.index_only);
        assert!(
            conv.warnings.is_empty(),
            "no warnings expected: {:?}",
            conv.warnings
        );
    }

    #[test]
    fn from_docs_query_round_trips_via_display() {
        let q = DocsQuery {
            tags: vec!["a".into(), "b".into()],
            folder: Some("kb-research".into()),
            caps: vec![Capability::Code],
            index_only: true,
            ..Default::default()
        };
        let e = Expr::from_docs_query(&q, Some("rust patterns"));
        let s = e.to_string();
        // Re-parse and compare the linearised view — the order in
        // from_docs_query is deterministic (tags first, then folder,
        // caps, index, text).
        let e2 = p(&s);
        let conv = e2.to_docs_query();
        assert_eq!(conv.query.tags, q.tags);
        assert_eq!(conv.query.folder, q.folder);
        assert_eq!(conv.query.caps, q.caps);
        assert_eq!(conv.query.index_only, q.index_only);
        assert_eq!(conv.text.as_deref(), Some("rust patterns"));
    }

    #[test]
    fn or_lowers_to_alternatives() {
        let e = p("tag:a OR tag:b");
        let conv = e.to_docs_query();
        // First branch fills the base; the rest become alternatives —
        // no more lossy collapse, no warning.
        assert_eq!(conv.query.tags, vec!["a".to_string()]);
        assert_eq!(conv.query.alternatives.len(), 1);
        assert_eq!(conv.query.alternatives[0].tags, vec!["b".to_string()]);
        assert!(conv.warnings.is_empty(), "no warnings: {:?}", conv.warnings);
    }

    #[test]
    fn not_tag_sets_exclude_tags() {
        let conv = p("NOT tag:draft").to_docs_query();
        assert_eq!(conv.query.exclude_tags, vec!["draft".to_string()]);
        assert!(conv.query.tags.is_empty());
        assert!(conv.query.alternatives.is_empty());
        assert!(conv.warnings.is_empty(), "{:?}", conv.warnings);
    }

    #[test]
    fn and_with_not_tag_includes_and_excludes() {
        // `tag:rust NOT tag:draft` → include rust, exclude draft, one conjunct.
        let conv = p("tag:rust NOT tag:draft").to_docs_query();
        assert_eq!(conv.query.tags, vec!["rust".to_string()]);
        assert_eq!(conv.query.exclude_tags, vec!["draft".to_string()]);
        assert!(conv.query.alternatives.is_empty());
        assert!(conv.warnings.is_empty(), "{:?}", conv.warnings);
    }

    #[test]
    fn and_of_or_distributes_into_conjuncts() {
        // (tag:a OR tag:b) AND folder:foo → two conjuncts, both carrying folder.
        let conv = p("(tag:a OR tag:b) AND folder:foo").to_docs_query();
        assert_eq!(conv.query.tags, vec!["a".to_string()]);
        assert_eq!(conv.query.folder.as_deref(), Some("foo"));
        assert_eq!(conv.query.alternatives.len(), 1);
        assert_eq!(conv.query.alternatives[0].tags, vec!["b".to_string()]);
        assert_eq!(conv.query.alternatives[0].folder.as_deref(), Some("foo"));
        assert!(conv.warnings.is_empty(), "{:?}", conv.warnings);
    }

    #[test]
    fn de_morgan_not_group_excludes_all() {
        // NOT (tag:a OR tag:b) = NOT tag:a AND NOT tag:b → one conjunct,
        // two excludes, no alternatives.
        let conv = p("NOT (tag:a OR tag:b)").to_docs_query();
        assert!(
            conv.query.alternatives.is_empty(),
            "single conjunct expected, got {:?}",
            conv.query.alternatives
        );
        assert_eq!(
            conv.query.exclude_tags,
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn dnf_expansion_caps_and_warns() {
        // 7 AND-ed OR-groups of 2 = 2^7 = 128 conjuncts > 64 cap → warn +
        // truncate to 64 total (base + 63 alternatives).
        let q = "(tag:a OR tag:b) (tag:c OR tag:d) (tag:e OR tag:f) (tag:g OR tag:h) \
                 (tag:i OR tag:j) (tag:k OR tag:l) (tag:m OR tag:n)";
        let conv = p(q).to_docs_query();
        assert!(
            conv.warnings.iter().any(|w| w.contains("capped")),
            "expected a cap warning: {:?}",
            conv.warnings
        );
        assert_eq!(conv.query.alternatives.len(), MAX_DNF_CONJUNCTS - 1);
    }

    #[test]
    fn not_since_still_warns() {
        // `since` has no in-memory upper-bound predicate, so NOT since
        // stays an honest warning rather than silently misfiltering.
        let conv = p("NOT since:7d").to_docs_query();
        assert!(
            conv.warnings.iter().any(|w| w.contains("since")),
            "expected a since warning: {:?}",
            conv.warnings
        );
        assert!(conv.query.since_unix.is_none());
    }

    #[test]
    fn since_relative_forms_compute_a_unix_timestamp() {
        let now = chrono::Utc::now().timestamp();
        let e = p("since:7d");
        let conv = e.to_docs_query();
        let t = conv.query.since_unix.expect("since_unix");
        let want = now - 7 * 86_400;
        // Allow a small drift since chrono::Utc::now() ticks during the test.
        assert!(
            (t - want).abs() < 3,
            "expected {want}, got {t} (drift {})",
            t - want
        );
    }

    #[test]
    fn since_unix_value_round_trips() {
        // The canonical round-trip form is a bare integer (no `unix:`
        // prefix — that would put a second colon in the value and the
        // tokenizer would split it).
        let e = p("since:1700000000");
        let conv = e.to_docs_query();
        assert_eq!(conv.query.since_unix, Some(1_700_000_000));
    }

    #[test]
    fn to_url_params_emits_legacy_query_shape() {
        let e = p("tag:a tag:b cap:code folder:foo index:true");
        let params = e.to_url_params();
        let map: std::collections::BTreeMap<_, _> = params.into_iter().collect();
        assert_eq!(map.get("tags").map(String::as_str), Some("a,b"));
        assert_eq!(map.get("folder").map(String::as_str), Some("foo"));
        assert_eq!(map.get("caps").map(String::as_str), Some("code"));
        assert_eq!(map.get("index").map(String::as_str), Some("1"));
    }

    #[test]
    fn caps_aliases_singular_and_plural() {
        // The sidebar label is `caps:`; user might type `cap:`. Both work.
        let a = p("cap:code");
        let b = p("caps:code");
        let conv_a = a.to_docs_query();
        let conv_b = b.to_docs_query();
        assert_eq!(conv_a.query.caps, conv_b.query.caps);
    }

    #[test]
    fn parse_rejects_deeply_nested_input_instead_of_overflowing() {
        // The query string is request-supplied; a hostile `((((…))))` or
        // `NOT NOT …` chain must error rather than overflow the parser
        // stack. ≤64 levels parse; well past the cap is rejected.
        let shallow = format!("{}tag:x{}", "(".repeat(60), ")".repeat(60));
        assert!(parse(&shallow).is_ok(), "60 levels should still parse");
        let deep = format!("{}tag:x{}", "(".repeat(200), ")".repeat(200));
        assert!(
            matches!(parse(&deep), Err(ParseError::TooDeep)),
            "200 nested groups must be TooDeep"
        );
        let nots = format!("{}tag:x", "NOT ".repeat(200));
        assert!(
            matches!(parse(&nots), Err(ParseError::TooDeep)),
            "200 NOTs must be TooDeep"
        );
    }
}
