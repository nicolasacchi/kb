//! `kbc-review/1` REFS — the scheme-prefixed reference grammar (V73-K1,
//! design D9).
//!
//! A ref is a `[[…]]` span whose body starts with one of seven CLOSED
//! scheme prefixes. Everything else inside `[[…]]` is a **kb wikilink** and
//! is left as literal text — root `CLAUDE.md` invariant #29 owns that
//! syntax and a kbc ref may never shadow it. That rule is why every kbc ref
//! carries a prefix in the first place: `[[Order]]` is kb's, `[[ent:Order]]`
//! is ours, and no reader (human, SPA, or this parser) has to guess.
//!
//! # The grammar
//!
//! | scheme | form | notes |
//! |---|---|---|
//! | `code` | `code:<path>[:<line>[-<end>]][@<sha>]` | the `@sha` pins the blob the author read |
//! | `sym` | `sym:<Qualified>[#<member>]` | `::` is never a field boundary |
//! | `ent` | `ent:<Fqn>` | a Ruby constant path, `entities/1`'s own address |
//! | `finding` | `finding:<slug>` | `f-…`, this review's own slug space |
//! | `gh` | `gh:<comment\|review\|issue\|pr>/<id>` | inert — kb-code never calls GitHub |
//! | `kb` | `kb:<kb>/<id>` | inert — a link into the kb corpus |
//! | `hunk` | `hunk:<path>@<ps>#<n>` | `ps` may be written `3` or `ps3` |
//! | `ci` | `ci:<check-name>` | inert — a snapshot read, never a live GitHub call (V73-K5) |
//! | `question` | `question:<n>` | this document's own `questions[]`, 1-based (V73-K5) |
//!
//! # Two parsing surfaces, one grammar
//!
//! [`parse_ref`] parses ONE body (`code:app/x.rb:12`) — the form a typed
//! front-matter field holds (a reading-order stop, a flow step, a
//! question's location, a finding's `cites`). [`scan_refs`] walks a whole
//! Markdown body and returns every `[[…]]` it finds, classified. Both go
//! through the same [`parse_ref`], so a ref cannot mean one thing in prose
//! and another in front-matter.
//!
//! # What the scanner skips
//!
//! Fenced code blocks (``` / ~~~, matching fence length and info string),
//! inline code spans (matching backtick runs) and the leading YAML
//! front-matter region. A ref written inside a fence is a ref being TALKED
//! ABOUT, not a ref being MADE — the same discipline kb's
//! `links::prose_text` applies to wikilinks. Four-space indented code
//! blocks are deliberately NOT skipped: they are indistinguishable from a
//! deeply nested list item without a full block parse, and silently
//! dropping a ref an author meant is worse than parsing one they meant as
//! prose (which `lint` then reports as an ordinary unresolvable ref).
//!
//! # Malformed is not a wikilink
//!
//! `[[code:]]` starts with a known scheme, so it is NOT handed back to kb's
//! wikilink grammar — it is reported as [`RefClass::Malformed`] with the
//! reason. Silently degrading a typo'd kbc ref into a kb wikilink would
//! make the failure invisible on both sides.
//!
//! # Golden
//!
//! `crates/kb-code-server/grammar/kbcrefs.golden.json` is ONE fixture read
//! by this module's `golden_corpus_matches_the_rust_parser` AND by
//! `web-code/src/lib/kbcRefs.golden.test.ts`, exactly as `kbcq.golden.json`
//! is shared between `search::grammar` and `web-code/src/lib/kbcq.ts`
//! (kb-code-server/CLAUDE.md invariant 16(a)). The fixture lives on the
//! CRATE side because the Rust builder stage's Docker context is
//! `COPY crates ./crates` — `web-code/` is not in it, so an `include_str!`
//! pointing the other way would not build.

use serde::Serialize;

/// The NINE scheme prefixes, in the order they are documented. A `[[…]]`
/// body whose text before the first `:` is NOT in this set is a kb
/// wikilink and this module never touches it.
///
/// V73-K5 added `ci` (a check-run snapshot citation) and `question` (a
/// document-local back-reference into this document's own `questions[]`)
/// — both closing the legacy-coverage gate's remaining schema gaps rather
/// than growing the grammar for its own sake.
pub const SCHEMES: [&str; 9] = [
    "code", "sym", "ent", "finding", "gh", "kb", "hunk", "ci", "question",
];

/// `gh:` object kinds — closed, so a typo is a malformed ref rather than a
/// link this daemon would have to guess the shape of.
pub const GH_KINDS: [&str; 4] = ["comment", "review", "issue", "pr"];

/// A parsed reference. `raw` is the body EXACTLY as written (without the
/// `[[`/`]]`), so every error message and every card can quote the author's
/// own text rather than a re-rendered approximation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "scheme", rename_all = "snake_case")]
pub enum Ref {
    Code {
        raw: String,
        path: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        line: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        line_end: Option<u32>,
        /// The blob the author read, as written (may be abbreviated).
        #[serde(skip_serializing_if = "Option::is_none")]
        sha: Option<String>,
    },
    Sym {
        raw: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        container: Option<String>,
        name: String,
    },
    Ent {
        raw: String,
        fqn: String,
    },
    Finding {
        raw: String,
        slug: String,
    },
    Gh {
        raw: String,
        kind: String,
        id: String,
    },
    Kb {
        raw: String,
        kb: String,
        id: String,
    },
    Hunk {
        raw: String,
        path: String,
        ps: i64,
        index: u32,
    },
    /// V73-K5 — `ci:<check-name>`. Resolved against a snapshot (the
    /// document's own authored `ci:` block, or one DERIVED from the
    /// review's stored `pr_meta_json.checks`) — never a live GitHub call,
    /// which is exactly why it is [`Ref::is_inert`].
    Ci {
        raw: String,
        name: String,
    },
    /// V73-K5 — `question:<n>`, a 1-based ordinal into THIS document's own
    /// `questions[]`. There is no persisted question id (unlike a finding's
    /// slug), so the ordinal into the SAME revision is the only stable
    /// address there is.
    Question {
        raw: String,
        index: u32,
    },
}

impl Ref {
    /// The body as written — the key every card, lint row and dedup uses.
    pub fn raw(&self) -> &str {
        match self {
            Ref::Code { raw, .. }
            | Ref::Sym { raw, .. }
            | Ref::Ent { raw, .. }
            | Ref::Finding { raw, .. }
            | Ref::Gh { raw, .. }
            | Ref::Kb { raw, .. }
            | Ref::Hunk { raw, .. }
            | Ref::Ci { raw, .. }
            | Ref::Question { raw, .. } => raw,
        }
    }

    /// The scheme name, as it appears in [`SCHEMES`].
    pub fn scheme(&self) -> &'static str {
        match self {
            Ref::Code { .. } => "code",
            Ref::Sym { .. } => "sym",
            Ref::Ent { .. } => "ent",
            Ref::Finding { .. } => "finding",
            Ref::Gh { .. } => "gh",
            Ref::Kb { .. } => "kb",
            Ref::Hunk { .. } => "hunk",
            Ref::Ci { .. } => "ci",
            Ref::Question { .. } => "question",
        }
    }

    /// `true` for the schemes this daemon deliberately does not resolve
    /// LIVE: `gh:`/`kb:` address something on the far side of a network
    /// call kb-code never makes or a corpus it does not own; `ci:` reads a
    /// SNAPSHOT (authored, or derived from `pr_meta_json.checks` at the
    /// last PR-metadata fetch) rather than calling GitHub's Checks API
    /// again — same "no live call" posture, so the same word applies.
    pub fn is_inert(&self) -> bool {
        matches!(self, Ref::Gh { .. } | Ref::Kb { .. } | Ref::Ci { .. })
    }
}

/// What a `[[…]]` span turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefClass {
    /// A well-formed kbc ref.
    Ref(Box<Ref>),
    /// A `[[…]]` with no known scheme prefix — kb's wikilink (#29). We
    /// carry the body so `lint` can report it as an informational "this is
    /// a kb link, not a kbc ref" rather than saying nothing.
    Wikilink(String),
    /// A `[[…]]` that DID start with a known scheme but does not parse.
    Malformed { body: String, reason: String },
}

/// One `[[…]]` span found by [`scan_refs`], with the position the author
/// can navigate to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundRef {
    pub class: RefClass,
    /// 1-based line within the WHOLE document text handed to [`scan_refs`].
    pub line: u32,
    /// 1-based column, counted in `char`s (what an editor shows), not bytes.
    pub col: u32,
}

/// Parse ONE ref body (no `[[`/`]]`). `Ok` for a well-formed ref, `Err`
/// with a reason for a body that names a known scheme but does not parse,
/// and `Ok(None)` for a body with no known scheme prefix at all (a kb
/// wikilink — never this module's business).
pub fn parse_ref(body: &str) -> Result<Option<Ref>, String> {
    let raw = body.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    let Some((scheme, rest)) = raw.split_once(':') else {
        return Ok(None);
    };
    if !SCHEMES.contains(&scheme) {
        return Ok(None);
    }
    let owned = raw.to_string();
    match scheme {
        "code" => parse_code(owned, rest).map(Some),
        "sym" => parse_sym(owned, rest).map(Some),
        "ent" => parse_ent(owned, rest).map(Some),
        "finding" => parse_finding(owned, rest).map(Some),
        "gh" => parse_gh(owned, rest).map(Some),
        "kb" => parse_kb(owned, rest).map(Some),
        "hunk" => parse_hunk(owned, rest).map(Some),
        "ci" => parse_ci(owned, rest).map(Some),
        "question" => parse_question(owned, rest).map(Some),
        // `SCHEMES` is the single declaration home; this arm is
        // unreachable while the `match` above covers it, and the test
        // `every_declared_scheme_has_a_parser_arm` fails by name if a
        // scheme is ever added to the list without one.
        other => Err(format!("no parser for scheme {other:?}")),
    }
}

/// `code:<path>[:<line>[-<end>]][@<sha>]`.
///
/// Parsed RIGHT to LEFT, because only the tail is unambiguous: a path may
/// contain `@` and `:` (rare, but legal on every filesystem this daemon
/// reads), while `@<hex>` at the very end and `:<digits>` immediately
/// before it cannot be anything else.
fn parse_code(raw: String, rest: &str) -> Result<Ref, String> {
    let (head, sha) = match rest.rsplit_once('@') {
        Some((h, s)) if is_hexish(s) && !h.is_empty() => (h, Some(s.to_string())),
        _ => (rest, None),
    };
    let (path, line, line_end) = match split_line_suffix(head) {
        Some((p, lo, hi)) => (p, Some(lo), hi),
        None => (head, None, None),
    };
    if path.is_empty() {
        return Err("code ref has an empty path".to_string());
    }
    if let (Some(lo), Some(hi)) = (line, line_end) {
        if hi < lo {
            return Err(format!(
                "code ref line range {lo}-{hi} ends before it starts"
            ));
        }
    }
    Ok(Ref::Code {
        raw,
        path: path.to_string(),
        line,
        line_end,
        sha,
    })
}

/// `sym:<Qualified>[#<member>]`. `#` splits container from name; failing
/// that, the LAST `::` does; failing that, the whole body is a bare name.
/// `::` is never a field boundary in its own right — Rust's module paths
/// and Ruby's constant paths both use it, and treating it as one would
/// make `kb_core::config::ServerSection` unaddressable.
fn parse_sym(raw: String, rest: &str) -> Result<Ref, String> {
    let rest = rest.trim();
    if rest.is_empty() {
        return Err("sym ref has an empty symbol".to_string());
    }
    if let Some((container, name)) = rest.split_once('#') {
        if container.is_empty() || name.is_empty() {
            return Err(format!("sym ref {rest:?} has an empty side of '#'"));
        }
        return Ok(Ref::Sym {
            raw,
            container: Some(container.to_string()),
            name: name.to_string(),
        });
    }
    match rest.rsplit_once("::") {
        Some((container, name)) if !container.is_empty() && !name.is_empty() => Ok(Ref::Sym {
            raw,
            container: Some(container.to_string()),
            name: name.to_string(),
        }),
        Some(_) => Err(format!("sym ref {rest:?} has an empty side of '::'")),
        None => Ok(Ref::Sym {
            raw,
            container: None,
            name: rest.to_string(),
        }),
    }
}

/// `ent:<Fqn>` — validated only for SHAPE here (non-empty, no whitespace).
/// Whether it is a legal Ruby constant path is `entities::validate_ent`'s
/// call, made at RESOLUTION time so the grammar and the entity index's own
/// address rules stay one rule, not two.
fn parse_ent(raw: String, rest: &str) -> Result<Ref, String> {
    let fqn = rest.trim();
    if fqn.is_empty() {
        return Err("ent ref has an empty constant path".to_string());
    }
    if fqn.chars().any(char::is_whitespace) {
        return Err(format!("ent ref {fqn:?} contains whitespace"));
    }
    Ok(Ref::Ent {
        raw,
        fqn: fqn.to_string(),
    })
}

/// `finding:<slug>` — `f-` prefixed, the same shape
/// `review_findings::is_valid_finding_slug` enforces on the wire.
fn parse_finding(raw: String, rest: &str) -> Result<Ref, String> {
    let slug = rest.trim();
    if !crate::review_findings::is_valid_finding_slug(slug) {
        return Err(format!(
            "finding ref {slug:?} is not a valid finding slug (f-[a-z0-9-]+)"
        ));
    }
    Ok(Ref::Finding {
        raw,
        slug: slug.to_string(),
    })
}

/// `gh:<kind>/<id>` with `kind` from [`GH_KINDS`].
fn parse_gh(raw: String, rest: &str) -> Result<Ref, String> {
    let Some((kind, id)) = rest.split_once('/') else {
        return Err(format!(
            "gh ref {rest:?} must be <kind>/<id>, kind one of {}",
            GH_KINDS.join("|")
        ));
    };
    if !GH_KINDS.contains(&kind) {
        return Err(format!(
            "gh ref kind {kind:?} is not one of {}",
            GH_KINDS.join("|")
        ));
    }
    if id.is_empty() || id.chars().any(char::is_whitespace) {
        return Err(format!("gh ref {rest:?} has an empty or spaced id"));
    }
    Ok(Ref::Gh {
        raw,
        kind: kind.to_string(),
        id: id.to_string(),
    })
}

/// `kb:<kb>/<id>` — split on the FIRST `/`; a kb name never contains one.
fn parse_kb(raw: String, rest: &str) -> Result<Ref, String> {
    let Some((kb, id)) = rest.split_once('/') else {
        return Err(format!("kb ref {rest:?} must be <kb>/<id>"));
    };
    if kb.is_empty() || id.is_empty() {
        return Err(format!("kb ref {rest:?} has an empty kb or id"));
    }
    if rest.chars().any(char::is_whitespace) {
        return Err(format!("kb ref {rest:?} contains whitespace"));
    }
    Ok(Ref::Kb {
        raw,
        kb: kb.to_string(),
        id: id.to_string(),
    })
}

/// `hunk:<path>@<ps>#<n>`. Parsed right to left for the same reason
/// [`parse_code`] is: only the tail is unambiguous.
fn parse_hunk(raw: String, rest: &str) -> Result<Ref, String> {
    let Some((head, idx)) = rest.rsplit_once('#') else {
        return Err(format!("hunk ref {rest:?} must end in #<n>"));
    };
    let index: u32 = idx
        .parse()
        .map_err(|_| format!("hunk ref index {idx:?} is not a number"))?;
    let Some((path, ps_raw)) = head.rsplit_once('@') else {
        return Err(format!("hunk ref {rest:?} must name a patchset (@<ps>)"));
    };
    let ps_digits = ps_raw.strip_prefix("ps").unwrap_or(ps_raw);
    let ps: i64 = ps_digits
        .parse()
        .map_err(|_| format!("hunk ref patchset {ps_raw:?} is not a number"))?;
    if path.is_empty() {
        return Err("hunk ref has an empty path".to_string());
    }
    if ps < 1 {
        return Err(format!("hunk ref patchset {ps} must be >= 1"));
    }
    Ok(Ref::Hunk {
        raw,
        path: path.to_string(),
        ps,
        index,
    })
}

/// `ci:<check-name>`. The name is whatever GitHub's Checks API calls the
/// run — free text, routinely containing spaces, slashes and parens (e.g.
/// `"test (3.11, ubuntu-latest)"`) — so the only refusal is an empty name;
/// nothing else about a check NAME is this grammar's business (whether it
/// is a check this review's snapshot actually KNOWS ABOUT is a resolution
/// question, answered honestly by the card's caption, never a parse error).
fn parse_ci(raw: String, rest: &str) -> Result<Ref, String> {
    let name = rest.trim();
    if name.is_empty() {
        return Err("ci ref has an empty check name".to_string());
    }
    Ok(Ref::Ci {
        raw,
        name: name.to_string(),
    })
}

/// `question:<n>`, `n >= 1` — a 1-based ordinal into the CURRENT document's
/// own `questions[]`. There is no cross-document form: a question has no
/// slug, so "which document" is always "the one this ref is read from".
fn parse_question(raw: String, rest: &str) -> Result<Ref, String> {
    let rest = rest.trim();
    let index: u32 = rest
        .parse()
        .map_err(|_| format!("question ref {rest:?} is not a positive whole number"))?;
    if index == 0 {
        return Err("question ref index must be >= 1 (questions are numbered from 1)".to_string());
    }
    Ok(Ref::Question { raw, index })
}

/// `…:<line>` or `…:<lo>-<hi>` at the very END of `s`; `None` when the tail
/// is not a line suffix (so the whole string is a path).
fn split_line_suffix(s: &str) -> Option<(&str, u32, Option<u32>)> {
    let (head, tail) = s.rsplit_once(':')?;
    if head.is_empty() {
        return None;
    }
    match tail.split_once('-') {
        Some((lo, hi)) => {
            let lo: u32 = lo.parse().ok()?;
            let hi: u32 = hi.parse().ok()?;
            Some((head, lo, Some(hi)))
        }
        None => {
            let lo: u32 = tail.parse().ok()?;
            Some((head, lo, None))
        }
    }
}

/// A plausible abbreviated git object name: 4–64 hex digits. Deliberately
/// permissive on LENGTH (git itself accepts any unambiguous abbreviation)
/// and strict on ALPHABET, so `a@b.rb` is never mistaken for a pinned sha.
fn is_hexish(s: &str) -> bool {
    (4..=64).contains(&s.len()) && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Walk a whole Markdown document and return every `[[…]]` span, skipping
/// fenced code blocks, inline code spans and a leading YAML front-matter
/// region. See the module doc for exactly what is skipped and why.
pub fn scan_refs(doc: &str) -> Vec<FoundRef> {
    let mut out = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    let mut in_front_matter = false;
    for (idx, line) in doc.lines().enumerate() {
        let lineno = (idx + 1) as u32;
        let trimmed = line.trim_start();

        // Front matter: a `---` on the VERY FIRST line opens it; the next
        // `---`/`...` at column 0 closes it. Anything else `---` is a
        // horizontal rule and means nothing here.
        if idx == 0 && line.trim_end() == "---" {
            in_front_matter = true;
            continue;
        }
        if in_front_matter {
            if line == "---" || line == "..." {
                in_front_matter = false;
            }
            continue;
        }

        if let Some((ch, len)) = fence {
            if is_closing_fence(trimmed, ch, len) {
                fence = None;
            }
            continue;
        }
        if let Some(open) = opening_fence(trimmed) {
            fence = Some(open);
            continue;
        }
        scan_line(line, lineno, &mut out);
    }
    out
}

/// Every PROSE line of `doc` (front matter, fenced code blocks and inline
/// code spans removed), as `(1-based document line, text)`. Inline spans are
/// replaced by spaces rather than deleted so column numbers still line up
/// with the author's own file.
///
/// Shared by [`scan_refs`]'s sibling consumers — `lint`'s bare-CapWord probe
/// reads THESE lines, so "what counts as prose" is decided once and cannot
/// drift between the ref scanner and the lint that comments on it.
pub fn prose_lines(doc: &str) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    let mut in_front_matter = false;
    for (idx, line) in doc.lines().enumerate() {
        let lineno = (idx + 1) as u32;
        let trimmed = line.trim_start();
        if idx == 0 && line.trim_end() == "---" {
            in_front_matter = true;
            continue;
        }
        if in_front_matter {
            if line == "---" || line == "..." {
                in_front_matter = false;
            }
            continue;
        }
        if let Some((ch, len)) = fence {
            if is_closing_fence(trimmed, ch, len) {
                fence = None;
            }
            continue;
        }
        if let Some(open) = opening_fence(trimmed) {
            fence = Some(open);
            continue;
        }
        out.push((lineno, blank_code_spans(line)));
    }
    out
}

/// Replace every inline code span (matching backtick runs) with spaces of
/// the same width.
fn blank_code_spans(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out: Vec<char> = chars.clone();
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] != '`' {
            i += 1;
            continue;
        }
        let run = chars[i..].iter().take_while(|c| **c == '`').count();
        let mut j = i + run;
        let mut closed = None;
        while j < chars.len() {
            if chars[j] == '`' {
                let r = chars[j..].iter().take_while(|c| **c == '`').count();
                if r == run {
                    closed = Some(j + r);
                    break;
                }
                j += r;
            } else {
                j += 1;
            }
        }
        match closed {
            Some(end) => {
                for slot in out.iter_mut().take(end).skip(i) {
                    *slot = ' ';
                }
                i = end;
            }
            None => i += run,
        }
    }
    out.into_iter().collect()
}

fn opening_fence(trimmed: &str) -> Option<(char, usize)> {
    for ch in ['`', '~'] {
        let run = trimmed.chars().take_while(|c| *c == ch).count();
        if run >= 3 {
            // An opening ``` fence's info string may not contain a
            // backtick (CommonMark); ~~~ has no such rule. Not enforced
            // here — a line of three or more fence characters opens a
            // fence, which is the only shape this scanner needs to be
            // right about.
            return Some((ch, run));
        }
    }
    None
}

fn is_closing_fence(trimmed: &str, ch: char, len: usize) -> bool {
    let run = trimmed.chars().take_while(|c| *c == ch).count();
    run >= len && trimmed[run..].trim().is_empty()
}

/// Scan one prose line, honouring inline code spans (matching backtick
/// runs, CommonMark's rule) and collecting every `[[…]]`.
fn scan_line(line: &str, lineno: u32, out: &mut Vec<FoundRef>) {
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] == '`' {
            let run = chars[i..].iter().take_while(|c| **c == '`').count();
            // Find the matching closing run of exactly the same length.
            let mut j = i + run;
            let mut closed = None;
            while j < chars.len() {
                if chars[j] == '`' {
                    let r = chars[j..].iter().take_while(|c| **c == '`').count();
                    if r == run {
                        closed = Some(j + r);
                        break;
                    }
                    j += r;
                } else {
                    j += 1;
                }
            }
            // An unterminated backtick run is literal text (CommonMark),
            // so we only skip when a real span closed.
            i = closed.unwrap_or(i + run);
            continue;
        }
        if chars[i] == '[' && i + 1 < chars.len() && chars[i + 1] == '[' {
            if let Some(close) = find_close(&chars, i + 2) {
                let body: String = chars[i + 2..close].iter().collect();
                out.push(FoundRef {
                    class: classify(&body),
                    line: lineno,
                    col: (i + 1) as u32,
                });
                i = close + 2;
                continue;
            }
        }
        i += 1;
    }
}

/// The first `]]` at or after `from`, or `None` for an unterminated `[[`.
/// A `]]` is never escaped in this grammar (an author who needs a literal
/// `[[` writes it in a code span, which the scanner already skips).
fn find_close(chars: &[char], from: usize) -> Option<usize> {
    let mut k = from;
    while k + 1 < chars.len() {
        if chars[k] == ']' && chars[k + 1] == ']' {
            return Some(k);
        }
        k += 1;
    }
    None
}

/// One `[[…]]` body → its class. The three outcomes are exhaustive and
/// disjoint: a kbc ref, a kb wikilink, or a typo'd kbc ref.
pub fn classify(body: &str) -> RefClass {
    match parse_ref(body) {
        Ok(Some(r)) => RefClass::Ref(Box::new(r)),
        Ok(None) => RefClass::Wikilink(body.to_string()),
        Err(reason) => RefClass::Malformed {
            body: body.to_string(),
            reason,
        },
    }
}

/// Every well-formed ref in `doc`, in document order, deduplicated by
/// `raw` (the same ref cited twice is one card). Used by the read route
/// and by `lint`, so the two can never disagree about what a document
/// cites.
pub fn refs_in(doc: &str) -> Vec<Ref> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out = Vec::new();
    for f in scan_refs(doc) {
        if let RefClass::Ref(r) = f.class {
            if seen.insert(r.raw().to_string()) {
                out.push(*r);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOLDEN: &str = include_str!("../../grammar/kbcrefs.golden.json");

    #[test]
    fn every_declared_scheme_has_a_parser_arm() {
        // A scheme in `SCHEMES` with no arm in `parse_ref` would make every
        // ref of that scheme a silent kb wikilink (`Ok(None)`) or an
        // internal error — the v7.0 dead-surface defect, in this grammar's
        // own shape. Probe each scheme with a body that is at least
        // scheme-shaped and assert the parser CLAIMS it (Ok(Some) or Err),
        // never disowns it (Ok(None)).
        for s in SCHEMES {
            let probe = format!("{s}:x/1#1@abcd");
            let got = parse_ref(&probe);
            assert!(
                !matches!(got, Ok(None)),
                "{s:?} is declared in SCHEMES but parse_ref disowns {probe:?} as a wikilink"
            );
        }
    }

    #[test]
    fn a_bare_wikilink_is_never_a_kbc_ref() {
        for body in ["Order", "Order|the order model", "docs/design", "a:b"] {
            assert_eq!(
                parse_ref(body),
                Ok(None),
                "{body:?} must stay a kb wikilink (root invariant #29)"
            );
            assert!(matches!(classify(body), RefClass::Wikilink(_)));
        }
    }

    #[test]
    fn a_known_scheme_that_does_not_parse_is_malformed_not_a_wikilink() {
        for body in [
            "code:",
            "gh:nope/1",
            "hunk:a.rb@2",
            "finding:F-7",
            "kb:solo",
        ] {
            match classify(body) {
                RefClass::Malformed { .. } => {}
                other => panic!("{body:?} classified as {other:?}, expected Malformed"),
            }
        }
    }

    #[test]
    fn code_ref_is_parsed_right_to_left() {
        let r = parse_ref("code:app/models/order.rb:120-134@a1b2c3d")
            .unwrap()
            .unwrap();
        assert_eq!(
            r,
            Ref::Code {
                raw: "code:app/models/order.rb:120-134@a1b2c3d".into(),
                path: "app/models/order.rb".into(),
                line: Some(120),
                line_end: Some(134),
                sha: Some("a1b2c3d".into()),
            }
        );
        // A path containing '@' is still a path when the tail is not hex.
        let r = parse_ref("code:app/mail@er.rb").unwrap().unwrap();
        assert!(matches!(r, Ref::Code { ref path, sha: None, .. } if path == "app/mail@er.rb"));
    }

    #[test]
    fn sym_ref_never_splits_on_a_double_colon_as_a_field_boundary() {
        let r = parse_ref("sym:kb_core::config::ServerSection")
            .unwrap()
            .unwrap();
        assert_eq!(
            r,
            Ref::Sym {
                raw: "sym:kb_core::config::ServerSection".into(),
                container: Some("kb_core::config".into()),
                name: "ServerSection".into(),
            }
        );
        let r = parse_ref("sym:Namespace::Class#method").unwrap().unwrap();
        assert_eq!(
            r,
            Ref::Sym {
                raw: "sym:Namespace::Class#method".into(),
                container: Some("Namespace::Class".into()),
                name: "method".into(),
            }
        );
    }

    #[test]
    fn scanner_skips_fences_and_code_spans_and_front_matter() {
        let doc = "---\nsummary_md: see [[code:fm.rb:1]]\n---\n\
                   prose [[code:a.rb:1]] and `[[code:span.rb:1]]`\n\
                   ```\n[[code:fenced.rb:1]]\n```\n\
                   after [[ent:Order]]\n";
        let found: Vec<String> = refs_in(doc).iter().map(|r| r.raw().to_string()).collect();
        assert_eq!(found, vec!["code:a.rb:1", "ent:Order"]);
    }

    #[test]
    fn scanner_reports_line_and_column_of_each_span() {
        let found = scan_refs("x\ny [[ent:Order]]\n");
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].line, found[0].col), (2, 3));
    }

    #[test]
    fn refs_in_dedups_by_raw_and_keeps_document_order() {
        let doc = "[[ent:B]] [[ent:A]] [[ent:B]]";
        let got: Vec<String> = refs_in(doc).iter().map(|r| r.raw().to_string()).collect();
        assert_eq!(got, vec!["ent:B", "ent:A"]);
    }

    // --- the LOCK-STEP golden ------------------------------------------

    #[derive(serde::Deserialize)]
    struct GoldenFile {
        schema: String,
        cases: Vec<GoldenCase>,
    }

    #[derive(serde::Deserialize)]
    struct GoldenCase {
        body: String,
        /// "ref" | "wikilink" | "malformed"
        class: String,
        #[serde(default)]
        parsed: Option<serde_json::Value>,
    }

    #[test]
    fn golden_corpus_matches_the_rust_parser() {
        let g: GoldenFile = serde_json::from_str(GOLDEN).expect("golden parses");
        assert_eq!(g.schema, "kbc-refs/1");
        assert!(!g.cases.is_empty());
        for c in &g.cases {
            match (classify(&c.body), c.class.as_str()) {
                (RefClass::Ref(r), "ref") => {
                    let got = serde_json::to_value(&*r).expect("ref serializes");
                    assert_eq!(
                        Some(&got),
                        c.parsed.as_ref(),
                        "golden case {:?} parsed differently",
                        c.body
                    );
                }
                (RefClass::Wikilink(_), "wikilink") => {}
                (RefClass::Malformed { .. }, "malformed") => {}
                (got, want) => panic!("golden case {:?}: got {got:?}, want {want:?}", c.body),
            }
        }
    }

    #[test]
    fn golden_covers_every_declared_scheme() {
        let g: GoldenFile = serde_json::from_str(GOLDEN).expect("golden parses");
        for s in SCHEMES {
            assert!(
                g.cases.iter().any(|c| {
                    c.class == "ref" && c.body.starts_with(&format!("{s}:"))
                }),
                "the golden corpus has no well-formed {s:?} case — add one when you touch the grammar"
            );
        }
    }
}
