//! V72-G1.1 — the per-request Ruby BODY scanners the entity dossier reads.
//!
//! Everything in this file is pure: it takes one file's bytes plus that
//! file's already-extracted [`Symbol`] rows and returns claims about the
//! lines inside ONE definition block. Nothing is persisted (crate
//! invariant 13's posture, and root CLAUDE.md #2's: kb-code mints classes
//! per request and caches none of them).
//!
//! ## Why a line scan at all
//!
//! tree-sitter-ruby's `tags.scm` captures methods, classes and modules —
//! and nothing else. An `attr_accessor :total`, a `TAX = 0.22`, an
//! `alias_method :sum, :total` and a `private` section are all ordinary
//! method CALLS to the Ruby grammar, so the symbols table cannot see them,
//! and a member table that omitted them would be wrong in the most
//! ordinary Rails file there is. The scanners below read those four shapes
//! off the DIRECT BODY LINES of a definition block — the lines inside the
//! block that no nested symbol covers — and everything they mint is capped
//! at `likely`, because a line scan proves less than a tree does.
//!
//! ## What "direct body line" means, and why the opener line survives
//!
//! [`direct_body_lines`] removes, for every symbol nested directly in the
//! block, the range `line_start + 1 ..= line_end` — the body, but NOT the
//! opener. That is deliberate: `private def total` and `attr_reader :a`
//! both live on an opener line, and dropping it would lose the visibility
//! modifier attached to the definition it modifies. A `private` sitting
//! inside a method body is inside the removed range and can never flip the
//! block's visibility, which is the failure mode this shape exists to
//! prevent.
//!
//! ## Visibility is resolved, or it is `unknown` — never guessed
//!
//! Ruby's `private` is a method call whose effect depends on where it is
//! evaluated. [`VisibilityScan`] tracks a keyword-block depth over the
//! direct body lines and REFUSES (every later member becomes
//! [`VIS_UNKNOWN`], with the reason recorded) the moment a bare visibility
//! keyword appears at a depth this scanner cannot account for — inside an
//! `included do … end`, an `if`, a `begin`, or behind a splat. A wrong
//! `private` on a member table is the member-level shape of a wrong
//! `exact`.

use crate::extract::Symbol;

/// The closed member-visibility vocabulary.
pub const VIS_PUBLIC: &str = "public";
pub const VIS_PROTECTED: &str = "protected";
pub const VIS_PRIVATE: &str = "private";
pub const VIS_MODULE_FUNCTION: &str = "module_function";
/// The honest outcome when a `private` keyword's scope cannot be resolved.
pub const VIS_UNKNOWN: &str = "unknown";

pub const VISIBILITIES: [&str; 5] = [
    VIS_PUBLIC,
    VIS_PROTECTED,
    VIS_PRIVATE,
    VIS_MODULE_FUNCTION,
    VIS_UNKNOWN,
];

/// Sort rank for `visibility` — the member table's primary key (the brief's
/// "sorted by visibility then name"). `unknown` sorts LAST: a row whose
/// visibility this scanner refused to claim must not be mixed into the
/// public block a reader skims first.
pub fn visibility_rank(v: &str) -> u8 {
    match v {
        VIS_PUBLIC => 0,
        VIS_PROTECTED => 1,
        VIS_PRIVATE => 2,
        VIS_MODULE_FUNCTION => 3,
        _ => 4,
    }
}

/// The closed member-kind vocabulary. `attr_*` are kept apart rather than
/// collapsed into one `attr`: reader/writer/accessor are three different
/// method sets, and a member table that said only "attr" would make a
/// caller re-read the source to learn which.
pub const MEMBER_KINDS: [&str; 7] = [
    "instance_method",
    "singleton_method",
    "attr_reader",
    "attr_writer",
    "attr_accessor",
    "constant",
    "alias",
];

/// How a member row was FOUND — surfaced rather than folded into `trust`,
/// for the same reason `EntityDefOut::nesting` is (a reader deserves to
/// know why a row is only `likely`).
pub const VIA_TREE: &str = "tree";
pub const VIA_MACRO: &str = "macro";
pub const VIA_ASSIGNMENT: &str = "assignment";

/// The closed metaprogramming-hole vocabulary of the `unknown_members`
/// lane. A mechanism with no detector would be the v7.0 dead-surface
/// defect, so every name here is minted by [`scan_unknown_members`] and a
/// test walks the list against the fixtures.
pub const MECH_DEFINE_METHOD: &str = "define_method";
pub const MECH_METHOD_MISSING: &str = "method_missing";
pub const MECH_RESPOND_TO_MISSING: &str = "respond_to_missing";
pub const MECH_DELEGATE: &str = "delegate";
pub const MECH_ATTR_DYNAMIC: &str = "attr_dynamic";
pub const MECH_CLASS_EVAL: &str = "class_eval";
pub const MECH_INSTANCE_EVAL: &str = "instance_eval";
pub const MECH_MODULE_EVAL: &str = "module_eval";
pub const MECH_SEND: &str = "send";
pub const MECH_PUBLIC_SEND: &str = "public_send";
pub const MECH_ALIAS_METHOD_DYNAMIC: &str = "alias_method_dynamic";

pub const UNKNOWN_MECHANISMS: [&str; 11] = [
    MECH_DEFINE_METHOD,
    MECH_METHOD_MISSING,
    MECH_RESPOND_TO_MISSING,
    MECH_DELEGATE,
    MECH_ATTR_DYNAMIC,
    MECH_CLASS_EVAL,
    MECH_INSTANCE_EVAL,
    MECH_MODULE_EVAL,
    MECH_SEND,
    MECH_PUBLIC_SEND,
    MECH_ALIAS_METHOD_DYNAMIC,
];

// --- line helpers -----------------------------------------------------------

/// The file's lines, 1-based indexable via `lines[n - 1]`. Lossy UTF-8 so a
/// file with one bad byte still yields a member table rather than nothing
/// (the bytes themselves are never echoed back — only trimmed line text).
pub fn lines_of(source: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(source)
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
        .collect()
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Byte column (0-based) of `word` in `line` as a whole word, or `None`.
/// Used to anchor the usages lane on the entity's own name rather than on
/// column 0 — `class OrderBase` must not answer a query for `Order`.
pub fn word_col(line: &str, word: &str) -> Option<u32> {
    if word.is_empty() {
        return None;
    }
    let hay = line.as_bytes();
    let needle = word.as_bytes();
    let mut i = 0usize;
    while i + needle.len() <= hay.len() {
        if &hay[i..i + needle.len()] == needle {
            let before_ok = i == 0 || !is_ident_byte(hay[i - 1]);
            let after = i + needle.len();
            let after_ok = after >= hay.len() || !is_ident_byte(hay[after]);
            if before_ok && after_ok {
                return Some(i as u32);
            }
        }
        i += 1;
    }
    None
}

/// The Ruby entity kinds a symbol row can carry (`crate::extract`'s
/// `map_kind` for Ruby). `singleton_class` is here because a `class <<
/// self` block CONTAINS members even though it names no entity.
pub fn is_container_kind(kind: &str) -> bool {
    matches!(kind, "class" | "module" | "singleton_class")
}

/// Symbols nested DIRECTLY inside `(start, end)` — no intervening
/// container. The same range-containment technique `defs_for_file` uses to
/// reconstruct nesting, applied one level down.
pub fn direct_children(symbols: &[Symbol], start: u32, end: u32) -> Vec<&Symbol> {
    let inside: Vec<&Symbol> = symbols
        .iter()
        .filter(|s| s.line_start > start && s.line_end <= end)
        .collect();
    inside
        .iter()
        .copied()
        .filter(|s| {
            !inside.iter().any(|t| {
                !std::ptr::eq(*t, *s)
                    && is_container_kind(&t.kind)
                    && t.line_start < s.line_start
                    && t.line_end >= s.line_end
            })
        })
        .collect()
}

/// 1-based line numbers of the block's own DIRECT body — see the module
/// doc for why a nested symbol's OPENER line survives. A `class << self`
/// block's INTERIOR is excluded too, for the reason
/// [`singleton_blocks`] records: the symbols table has no node for it, so
/// range containment alone would let its `private` section leak out into
/// the enclosing class.
pub fn direct_body_lines(lines: &[String], symbols: &[Symbol], start: u32, end: u32) -> Vec<u32> {
    let children = direct_children(symbols, start, end);
    let singles = singleton_blocks(lines, symbols, start, end);
    let last = end.min(lines.len() as u32);
    (start + 1..last)
        .filter(|n| {
            !children
                .iter()
                .any(|c| *n > c.line_start && *n <= c.line_end)
                && !singles.iter().any(|(a, b)| *n > *a && *n <= *b)
        })
        .collect()
}

/// `true` for a `class << self` opener line — and ONLY `self`. A
/// `class << Foo` block opens the singleton of some OTHER constant and its
/// members are that constant's, not this block's; the symbols table does
/// capture that form (tree-sitter-ruby's `singleton_class` pattern wants a
/// CONSTANT receiver), so it is left to ordinary range containment.
fn opens_self_singleton(line: &str) -> bool {
    let t = line.split('#').next().unwrap_or(line).trim();
    if first_token(t) != "class" {
        return false;
    }
    let rest = t["class".len()..].trim_start();
    let Some(rest) = rest.strip_prefix("<<") else {
        return false;
    };
    rest.trim() == "self"
}

/// The `class << self` blocks nested DIRECTLY in `(start, end)`, as
/// `(opener_line, end_line)` pairs.
///
/// tree-sitter-ruby's tags query does not capture `class << self` at all —
/// its `singleton_class` pattern binds `value:` to a CONSTANT, and `self`
/// is not one — so the symbols table holds no node for the block and
/// range containment cannot see it. Two things go wrong without this
/// recovery, and both are wrong ANSWERS rather than missing ones: every
/// `def` inside the block lands in the enclosing class as an
/// `instance_method` (it is a class method), and the block's own
/// `private` section leaks out and marks the enclosing class's later
/// members private. The end line is found by walking forward with a
/// keyword depth, consuming each nested SYMBOL's whole range in one step
/// so a `def`'s own `end` never decrements the count.
pub fn singleton_blocks(
    lines: &[String],
    symbols: &[Symbol],
    start: u32,
    end: u32,
) -> Vec<(u32, u32)> {
    let last = end.min(lines.len() as u32);
    let mut out: Vec<(u32, u32)> = Vec::new();
    let mut n = start + 1;
    while n < last {
        // A symbol starting here covers its whole range — skip it whole.
        if let Some(s) = symbols
            .iter()
            .filter(|s| s.line_start == n && s.line_end <= last)
            .max_by_key(|s| s.line_end)
        {
            n = s.line_end + 1;
            continue;
        }
        if out.iter().any(|(a, b)| n > *a && n <= *b) {
            n += 1;
            continue;
        }
        if opens_self_singleton(&lines[(n - 1) as usize]) {
            if let Some(e) = matching_end(lines, symbols, n, last) {
                out.push((n, e));
                n = e + 1;
                continue;
            }
        }
        n += 1;
    }
    out
}

/// The line closing a block opened at `open_line`, or `None` when the scan
/// runs past `limit` (a file this scanner cannot follow yields no
/// singleton block rather than a guessed range).
fn matching_end(lines: &[String], symbols: &[Symbol], open_line: u32, limit: u32) -> Option<u32> {
    let mut depth: i32 = 1;
    let mut n = open_line + 1;
    while n <= limit {
        if let Some(s) = symbols
            .iter()
            .filter(|s| s.line_start == n && s.line_end <= limit)
            .max_by_key(|s| s.line_end)
        {
            n = s.line_end + 1;
            continue;
        }
        let line = &lines[(n - 1) as usize];
        if closes_keyword_block(line) {
            depth -= 1;
            if depth == 0 {
                return Some(n);
            }
        } else if opens_keyword_block(line)
            || matches!(first_token(line.trim()), "class" | "module" | "def")
        {
            depth += 1;
        }
        n += 1;
    }
    None
}

// --- visibility -------------------------------------------------------------

/// The resolved visibility of every direct body line of one block, plus the
/// honest refusal state.
#[derive(Debug, Clone)]
pub struct VisibilityScan {
    /// Default visibility in force AT each scanned line, keyed by line.
    pub at_line: std::collections::BTreeMap<u32, &'static str>,
    /// Explicit per-name overrides (`private :total`, `private def total`).
    pub named: std::collections::BTreeMap<String, &'static str>,
    /// `Some` when a bare visibility keyword appeared somewhere this
    /// scanner cannot account for — every member at or after that line is
    /// [`VIS_UNKNOWN`].
    pub refused: Option<String>,
    /// Line from which [`Self::refused`] applies.
    pub refused_from: Option<u32>,
}

impl VisibilityScan {
    pub fn for_line(&self, line: u32) -> &'static str {
        if let Some(from) = self.refused_from {
            if line >= from {
                return VIS_UNKNOWN;
            }
        }
        self.at_line
            .range(..=line)
            .next_back()
            .map(|(_, v)| *v)
            .unwrap_or(VIS_PUBLIC)
    }

    pub fn for_member(&self, name: &str, line: u32) -> &'static str {
        // `.copied()` rather than a deref: the map's VALUE is already
        // `&'static str`, and returning the `&&'static str` the lookup
        // hands back would tie it to this borrow of `self`.
        if let Some(v) = self.named.get(name).copied() {
            return v;
        }
        self.for_line(line)
    }
}

fn first_token(line: &str) -> &str {
    let t = line.trim_start();
    let end = t
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '?' || c == '!'))
        .unwrap_or(t.len());
    &t[..end]
}

fn opens_keyword_block(line: &str) -> bool {
    let t = line.trim();
    if t.is_empty() || t.starts_with('#') {
        return false;
    }
    if matches!(
        first_token(t),
        "if" | "unless" | "while" | "until" | "case" | "begin" | "for"
    ) {
        return true;
    }
    // `included do`, `scope :x do |…|`, `ActiveSupport.on_load(:x) do`.
    let head = t.split('#').next().unwrap_or(t).trim_end();
    head == "do" || head.ends_with(" do") || (head.ends_with('|') && head.contains(" do |"))
}

fn closes_keyword_block(line: &str) -> bool {
    let t = line.trim();
    t == "end" || t.starts_with("end ") || t.starts_with("end#")
}

fn visibility_keyword(tok: &str) -> Option<&'static str> {
    match tok {
        "public" => Some(VIS_PUBLIC),
        "protected" => Some(VIS_PROTECTED),
        "private" => Some(VIS_PRIVATE),
        "module_function" => Some(VIS_MODULE_FUNCTION),
        _ => None,
    }
}

/// Parse a `:symbol`/`"string"` list off a macro call's argument text.
/// Returns `(names, all_literal)` — `all_literal` is `false` the moment an
/// argument is anything else (a splat, a constant, an interpolation),
/// which is what routes `attr_accessor(*NAMES)` to the unknown-members
/// lane instead of fabricating member rows.
pub fn literal_name_args(args: &str) -> (Vec<String>, bool) {
    let args = args.split('#').next().unwrap_or(args).trim();
    let args = args.strip_prefix('(').unwrap_or(args);
    let args = args.strip_suffix(')').unwrap_or(args);
    let mut names = Vec::new();
    let mut all_literal = !args.trim().is_empty();
    for raw in args.split(',') {
        let a = raw.trim();
        if a.is_empty() {
            continue;
        }
        // `to: :other` and friends are keyword args, not names.
        if a.contains(':') && !a.starts_with(':') {
            all_literal = false;
            continue;
        }
        if let Some(sym) = a.strip_prefix(':') {
            let name: String = sym
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '?' || *c == '!')
                .collect();
            if name.is_empty() || name.len() != sym.len() {
                all_literal = false;
            } else {
                names.push(name);
            }
        } else if (a.starts_with('"') && a.ends_with('"') && a.len() > 1)
            || (a.starts_with('\'') && a.ends_with('\'') && a.len() > 1)
        {
            names.push(a[1..a.len() - 1].to_string());
        } else {
            all_literal = false;
        }
    }
    (names, all_literal)
}

/// Split `attr_reader :a, :b` into `("attr_reader", ":a, :b")`.
fn macro_call(line: &str) -> Option<(&str, &str)> {
    let t = line.trim();
    if t.is_empty() || t.starts_with('#') {
        return None;
    }
    let name = first_token(t);
    if name.is_empty() {
        return None;
    }
    let rest = t[name.len()..].trim_start();
    Some((name, rest))
}

/// Resolve the visibility in force over one block's direct body lines.
pub fn scan_visibility(lines: &[String], body: &[u32]) -> VisibilityScan {
    let mut scan = VisibilityScan {
        at_line: std::collections::BTreeMap::new(),
        named: std::collections::BTreeMap::new(),
        refused: None,
        refused_from: None,
    };
    let mut depth: i32 = 0;
    for &n in body {
        let Some(line) = lines.get((n - 1) as usize) else {
            continue;
        };
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((name, rest)) = macro_call(line) else {
            continue;
        };
        // `private def total` — the modifier applies to that ONE method.
        if let Some(vis) = visibility_keyword(name) {
            if rest.is_empty() {
                if depth > 0 {
                    if scan.refused.is_none() {
                        scan.refused = Some(format!(
                            "a bare `{name}` at line {n} sits inside a block this scanner cannot \
                             account for — every member from here on is reported `unknown` rather \
                             than guessed"
                        ));
                        scan.refused_from = Some(n);
                    }
                } else {
                    scan.at_line.insert(n, vis);
                }
                continue;
            }
            if let Some(defrest) = rest.strip_prefix("def ") {
                let mname = def_name(defrest);
                if let Some(m) = mname {
                    scan.named.insert(m, vis);
                }
                continue;
            }
            let (names, all_literal) = literal_name_args(rest);
            if all_literal && !names.is_empty() {
                for m in names {
                    scan.named.insert(m, vis);
                }
            } else if scan.refused.is_none() {
                scan.refused = Some(format!(
                    "`{name}` at line {n} takes an argument this scanner cannot read as a literal \
                     name — every member from here on is reported `unknown` rather than guessed"
                ));
                scan.refused_from = Some(n);
            }
            continue;
        }
        if name == "private_class_method" || name == "public_class_method" {
            let vis = if name == "private_class_method" {
                VIS_PRIVATE
            } else {
                VIS_PUBLIC
            };
            let (names, all_literal) = literal_name_args(rest);
            if all_literal {
                for m in names {
                    scan.named.insert(m, vis);
                }
            }
            continue;
        }
        if opens_keyword_block(line) {
            depth += 1;
        } else if closes_keyword_block(line) {
            depth = (depth - 1).max(0);
        }
    }
    scan
}

/// `total(a, b)` / `self.total` → the bare method name.
pub fn def_name(rest: &str) -> Option<String> {
    let r = rest.trim();
    let r = r.strip_prefix("self.").unwrap_or(r);
    let name: String = r
        .chars()
        .take_while(|c| {
            c.is_ascii_alphanumeric() || *c == '_' || *c == '?' || *c == '!' || *c == '='
        })
        .collect();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

// --- macro / constant members ----------------------------------------------

/// One member found by a LINE scan rather than by the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedMember {
    pub name: String,
    pub kind: &'static str,
    pub via: &'static str,
    pub line: u32,
}

/// `attr_*`, `alias_method` and constant assignments on one block's direct
/// body lines. Dynamic argument forms are deliberately ABSENT here — they
/// belong to [`scan_unknown_members`], which names the hole instead of
/// inventing a member.
pub fn scan_scanned_members(lines: &[String], body: &[u32]) -> Vec<ScannedMember> {
    let mut out = Vec::new();
    for &n in body {
        let Some(line) = lines.get((n - 1) as usize) else {
            continue;
        };
        let Some((name, rest)) = macro_call(line) else {
            continue;
        };
        match name {
            "attr_reader" | "attr_writer" | "attr_accessor" => {
                let (names, all_literal) = literal_name_args(rest);
                if !all_literal {
                    continue;
                }
                for m in names {
                    out.push(ScannedMember {
                        name: m,
                        kind: match name {
                            "attr_reader" => "attr_reader",
                            "attr_writer" => "attr_writer",
                            _ => "attr_accessor",
                        },
                        via: VIA_MACRO,
                        line: n,
                    });
                }
            }
            "alias_method" => {
                let (names, all_literal) = literal_name_args(rest);
                // A dynamic `alias_method` is a HOLE, not a member —
                // `scan_unknown_members` names it.
                let alias_target = if all_literal { names.first() } else { None };
                if let Some(new) = alias_target {
                    out.push(ScannedMember {
                        name: new.clone(),
                        kind: "alias",
                        via: VIA_MACRO,
                        line: n,
                    });
                }
            }
            _ => {
                if let Some(c) = constant_assignment(line) {
                    out.push(ScannedMember {
                        name: c,
                        kind: "constant",
                        via: VIA_ASSIGNMENT,
                        line: n,
                    });
                }
            }
        }
    }
    out
}

/// `TAX_RATE = 0.22` → `Some("TAX_RATE")`. Rejects `==`, `<=`, `>=`, `!=`
/// and every non-constant left-hand side.
pub fn constant_assignment(line: &str) -> Option<String> {
    let t = line.trim();
    if t.is_empty() || t.starts_with('#') {
        return None;
    }
    let eq = t.find('=')?;
    let lhs = t[..eq].trim();
    let after = t.as_bytes().get(eq + 1).copied();
    if matches!(after, Some(b'=') | Some(b'~')) {
        return None;
    }
    if lhs.ends_with('!') || lhs.ends_with('<') || lhs.ends_with('>') || lhs.ends_with('=') {
        return None;
    }
    if lhs.is_empty() || !lhs.chars().next()?.is_ascii_uppercase() {
        return None;
    }
    if !lhs.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    Some(lhs.to_string())
}

// --- metaprogramming holes --------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedHole {
    pub mechanism: &'static str,
    pub name_hint: Option<String>,
    pub line: u32,
    pub context: String,
}

/// Every metaprogramming hole on ANY line of the block — not just the
/// direct body lines. A `define_method` inside `class << self` or inside
/// an `included do` block hides members just as effectively as one at the
/// top level, and the panel exists to say so.
pub fn scan_unknown_members(lines: &[String], start: u32, end: u32) -> Vec<ScannedHole> {
    let mut out = Vec::new();
    let last = end.min(lines.len() as u32);
    for n in start..=last {
        let Some(line) = lines.get((n - 1) as usize) else {
            continue;
        };
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let ctx = context_of(t);
        let mut push = |mechanism: &'static str, name_hint: Option<String>| {
            out.push(ScannedHole {
                mechanism,
                name_hint,
                line: n,
                context: ctx.clone(),
            })
        };
        if let Some(rest) = after_word(t, "define_method") {
            push(MECH_DEFINE_METHOD, first_literal_name(rest));
        }
        if after_word(t, "def method_missing").is_some() {
            push(MECH_METHOD_MISSING, Some("method_missing".to_string()));
        }
        if after_word(t, "def respond_to_missing?").is_some() {
            push(
                MECH_RESPOND_TO_MISSING,
                Some("respond_to_missing?".to_string()),
            );
        }
        if first_token(t) == "delegate" {
            let rest = t["delegate".len()..].trim();
            let (names, _) = literal_name_args(rest);
            push(MECH_DELEGATE, names.first().cloned());
        }
        if matches!(
            first_token(t),
            "attr_reader" | "attr_writer" | "attr_accessor"
        ) {
            let name = first_token(t).to_string();
            let rest = t[name.len()..].trim();
            let (_, all_literal) = literal_name_args(rest);
            if !all_literal {
                push(MECH_ATTR_DYNAMIC, None);
            }
        }
        if first_token(t) == "alias_method" {
            let rest = t["alias_method".len()..].trim();
            let (_, all_literal) = literal_name_args(rest);
            if !all_literal {
                push(MECH_ALIAS_METHOD_DYNAMIC, None);
            }
        }
        for (word, mech) in [
            ("class_eval", MECH_CLASS_EVAL),
            ("instance_eval", MECH_INSTANCE_EVAL),
            ("module_eval", MECH_MODULE_EVAL),
        ] {
            if after_word(t, word).is_some() {
                push(mech, None);
            }
        }
        for (word, mech) in [("public_send", MECH_PUBLIC_SEND), ("send", MECH_SEND)] {
            if let Some(rest) = after_word(t, word) {
                // `public_send` also matches `send`; keep the longer one only.
                if mech == MECH_SEND && after_word(t, "public_send").is_some() {
                    continue;
                }
                if let Some(name) = first_literal_name(rest) {
                    push(mech, Some(name));
                }
            }
        }
    }
    out.sort_by(|a, b| a.line.cmp(&b.line).then(a.mechanism.cmp(b.mechanism)));
    out
}

fn context_of(t: &str) -> String {
    const CAP: usize = 200;
    if t.len() <= CAP {
        return t.to_string();
    }
    let mut end = CAP;
    while end > 0 && !t.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &t[..end])
}

/// Text after a whole-word occurrence of `word`, or `None`.
fn after_word<'a>(line: &'a str, word: &str) -> Option<&'a str> {
    let col = word_col(line, word)? as usize;
    Some(&line[col + word.len()..])
}

/// `(:total, …)` / ` :total` / `("total") { … }` → `Some("total")`.
/// Deliberately NOT [`literal_name_args`]: this argument is followed by a
/// block, a comma or a closing paren, so the strict "the whole token is a
/// name" rule that keeps `attr_accessor(*NAMES)` out of the member table
/// would reject a perfectly readable hint here.
fn first_literal_name(rest: &str) -> Option<String> {
    let r = rest.trim_start();
    let r = r.strip_prefix('(').unwrap_or(r).trim_start();
    if let Some(sym) = r.strip_prefix(':') {
        let name: String = sym
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '?' || *c == '!')
            .collect();
        return if name.is_empty() { None } else { Some(name) };
    }
    for q in ['"', '\''] {
        if let Some(rest2) = r.strip_prefix(q) {
            let name = rest2.split(q).next().unwrap_or("");
            return if name.is_empty() {
                None
            } else {
                Some(name.to_string())
            };
        }
    }
    None
}

// --- opener form ------------------------------------------------------------

/// How a definition block spells its own name — D6's "`module A; class B`
/// vs `class A::B`" distinction, which is invisible in an FQN and is the
/// first thing a reader wants when an entity is reopened in five files.
pub const OPENER_TOP_LEVEL: &str = "top-level";
pub const OPENER_NESTED: &str = "nested";
pub const OPENER_COMPACT: &str = "compact";
pub const OPENER_MIXED: &str = "mixed";
pub const OPENER_UNKNOWN: &str = "unknown";

/// `(rendered, form)` for the block starting at `line` in a file whose
/// symbols are `symbols`. `rendered` is the literal opener chain joined by
/// `"; "` — `module Reseller; class Order` — read off the source, never
/// reconstructed from the FQN.
pub fn opener_form(lines: &[String], symbols: &[Symbol], line: u32) -> (String, &'static str) {
    let Some(own) = symbols
        .iter()
        .filter(|s| is_container_kind(&s.kind) && s.line_start == line)
        .min_by_key(|s| s.line_end - s.line_start)
    else {
        return (String::new(), OPENER_UNKNOWN);
    };
    let mut chain: Vec<&Symbol> = symbols
        .iter()
        .filter(|s| {
            is_container_kind(&s.kind)
                && s.line_start < own.line_start
                && s.line_end >= own.line_end
        })
        .collect();
    chain.sort_by_key(|s| s.line_start);
    chain.push(own);
    let mut rendered: Vec<String> = Vec::new();
    for s in &chain {
        let Some(text) = lines.get((s.line_start - 1) as usize) else {
            return (String::new(), OPENER_UNKNOWN);
        };
        let head = text.trim();
        let head = head.split(" < ").next().unwrap_or(head).trim();
        let head = head.split('#').next().unwrap_or(head).trim();
        rendered.push(head.to_string());
    }
    let own_line = lines
        .get((own.line_start - 1) as usize)
        .map(|s| s.trim())
        .unwrap_or("");
    let compact = own_line
        .split(" < ")
        .next()
        .unwrap_or(own_line)
        .contains("::");
    let nested = chain.len() > 1;
    let form = match (nested, compact) {
        (false, false) => OPENER_TOP_LEVEL,
        (true, false) => OPENER_NESTED,
        (false, true) => OPENER_COMPACT,
        (true, true) => OPENER_MIXED,
    };
    (rendered.join("; "), form)
}

// --- superclass + mixins ----------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MixinRef {
    /// `include` | `prepend` | `extend`.
    pub kind: &'static str,
    /// The constant AS WRITTEN (`Payable`, `::Shop::Payable`).
    pub written: String,
    pub line: u32,
}

/// The superclass written on a `class X < Y` opener line, verbatim.
pub fn superclass_of(lines: &[String], line: u32) -> Option<String> {
    let text = lines.get((line - 1) as usize)?;
    let head = text.split('#').next().unwrap_or(text).trim();
    if first_token(head) != "class" {
        return None;
    }
    let (_, rhs) = head.split_once(" < ")?;
    let name: String = rhs
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == ':')
        .collect();
    if name.is_empty() || !name.starts_with(|c: char| c.is_ascii_uppercase() || c == ':') {
        // `class Foo < Struct.new(:a)` — a runtime expression, not a name.
        return None;
    }
    Some(name)
}

/// `include`/`prepend`/`extend` on a block's DIRECT body lines. A mixin
/// applied inside an `included do` block belongs to the including class,
/// not to this one, which is exactly why the direct-body restriction
/// matters here.
pub fn mixins_of(lines: &[String], body: &[u32]) -> Vec<MixinRef> {
    let mut out = Vec::new();
    for &n in body {
        let Some(line) = lines.get((n - 1) as usize) else {
            continue;
        };
        let Some((name, rest)) = macro_call(line) else {
            continue;
        };
        let kind = match name {
            "include" => "include",
            "prepend" => "prepend",
            "extend" => "extend",
            _ => continue,
        };
        let written: String = rest
            .trim()
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == ':')
            .collect();
        if written.is_empty() || !written.starts_with(|c: char| c.is_ascii_uppercase() || c == ':')
        {
            continue;
        }
        out.push(MixinRef {
            kind,
            written,
            line: n,
        });
    }
    out
}
