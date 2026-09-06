//! `comments/1` — the ANNOTATION keyword grammar (D8's "configurable
//! keywords + smart_todo").
//!
//! Two things live here and nowhere else: the effective keyword SET
//! (RuboCop's six plus the two legacy markers, replaceable wholesale by
//! `[comments] keywords`), and the `TODO(on: …, to: '…')` parenthetical
//! parser that turns a smart_todo-shaped annotation into typed fields.
//!
//! ## Why the default set is eight, not RuboCop's six
//!
//! RuboCop's `Style/CommentAnnotation` ships `TODO FIXME OPTIMIZE HACK
//! REVIEW NOTE` and that is the vocabulary this index is designed around.
//! `XXX` and `BUG` are carried in the DEFAULT set for one concrete reason:
//! they are two of the five markers the pre-`comments/1` TODO index
//! (`extract::extract_todos`, deleted by this unit) scanned, and
//! `GET /api/todos` is now a filtered VIEW over this index. Dropping them
//! from the default would silently shrink that route's row set for every
//! existing consumer. An operator who sets `[comments] keywords` REPLACES
//! the whole set — including the two legacy markers — and the `/todos`
//! view narrows honestly along with it.
//!
//! ## Match rule (and how it differs from RuboCop's cop)
//!
//! RuboCop's cop is a STYLE rule about how an annotation should be
//! WRITTEN: keyword first in the comment, colon required (`RequireColon`).
//! An INDEX has the opposite job — find the annotations that exist, not
//! the ones that are well-formed. So a keyword matches anywhere on a
//! comment line at an ASCII word boundary, case-sensitively, colon
//! optional, and the LEFTMOST match on the line wins. (The deleted
//! `find_todo_marker` resolved a two-marker line by the declaration order
//! of its hardcoded array instead, so `// TODO: drop this FIXME shim`
//! reported `FIXME`. That is the one row-set change this unit makes to
//! `GET /api/todos`, and it is pinned by a test.)

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// RuboCop `Style/CommentAnnotation`'s own default `Keywords`, in its own
/// order. Exposed so `GET /api/comments/keywords` can report what an
/// operator's override departed from.
pub const RUBOCOP_KEYWORDS: &[&str] = &["TODO", "FIXME", "OPTIMIZE", "HACK", "REVIEW", "NOTE"];

/// The two markers the deleted `extract::TODO_MARKERS` scanned that
/// RuboCop's set does not name. See the module doc.
pub const LEGACY_EXTRA_KEYWORDS: &[&str] = &["XXX", "BUG"];

/// The keyword family `GET /api/todos` reports — exactly the five markers
/// the pre-`comments/1` index scanned, so that route's row set does not
/// silently gain `OPTIMIZE`/`REVIEW`/`NOTE` hits when this index starts
/// finding them.
pub const TODO_FAMILY: &[&str] = &["TODO", "FIXME", "HACK", "XXX", "BUG"];

/// The trailing-text cap for an annotation's own line, in chars — the
/// deleted `extract::TODO_TEXT_CAP`'s value, kept byte-for-byte because
/// `GET /api/todos`'s `text` field is this string.
pub const KEYWORD_TEXT_CAP: usize = 200;

/// The effective keyword set for this daemon: `[comments] keywords` when
/// the operator set one, else the default eight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeywordSet {
    /// Uppercase-as-written keywords, longest-first (so a hypothetical
    /// `TODOX` cannot be shadowed by `TODO` at the same offset).
    pub keywords: Vec<String>,
    /// `"default"` or `"config"` — reported verbatim by
    /// `GET /api/comments/keywords` so a surprising row set is one read
    /// away from its cause.
    pub source: &'static str,
}

impl Default for KeywordSet {
    fn default() -> Self {
        Self::defaults()
    }
}

impl KeywordSet {
    /// The shipped default: RuboCop's six then the two legacy markers.
    pub fn defaults() -> Self {
        let mut keywords: Vec<String> = RUBOCOP_KEYWORDS
            .iter()
            .chain(LEGACY_EXTRA_KEYWORDS.iter())
            .map(|k| (*k).to_string())
            .collect();
        sort_longest_first(&mut keywords);
        Self {
            keywords,
            source: "default",
        }
    }

    /// Resolve `[comments] keywords`. An override REPLACES the default set
    /// wholesale (it is not additive) — empty/blank entries are dropped,
    /// and a list that is entirely blank falls back to the defaults rather
    /// than silently indexing zero annotations.
    pub fn from_config(configured: &[String]) -> Self {
        let mut keywords: Vec<String> = configured
            .iter()
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
            .collect();
        if keywords.is_empty() {
            return Self::defaults();
        }
        keywords.sort();
        keywords.dedup();
        sort_longest_first(&mut keywords);
        Self {
            keywords,
            source: "config",
        }
    }

    /// A short, stable fingerprint of the effective set — half of the
    /// per-row `comments_version` key. FNV-1a over the canonical
    /// (longest-first, then alphabetical) rendering, so two configs that
    /// name the same keywords in a different order fingerprint the same.
    pub fn fingerprint(&self) -> String {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for kw in &self.keywords {
            for b in kw.as_bytes().iter().chain(std::iter::once(&0u8)) {
                hash ^= u64::from(*b);
                hash = hash.wrapping_mul(0x1000_0000_01b3);
            }
        }
        format!("kw{hash:08x}")
    }

    /// The LEFTMOST word-boundary keyword hit on one comment line, with
    /// the trailing text after it (trimmed, capped at
    /// [`KEYWORD_TEXT_CAP`] chars) and the smart_todo fields when the
    /// keyword is immediately followed by a `(`.
    pub fn find(&self, line: &str) -> Option<KeywordHit> {
        let mut best: Option<(usize, &str)> = None;
        for kw in &self.keywords {
            if let Some(at) = word_bounded_find(line, kw) {
                match best {
                    // Strictly-less keeps the FIRST keyword in
                    // longest-first order at a tie offset, which is what
                    // makes a longer keyword win over a prefix of itself.
                    Some((prev, _)) if prev <= at => {}
                    _ => best = Some((at, kw.as_str())),
                }
            }
        }
        let (at, kw) = best?;
        let after = &line[at + kw.len()..];
        Some(KeywordHit {
            // Everything after the keyword, INCLUDING a smart_todo
            // parenthetical. `fields` is a parsed VIEW of that same text,
            // never a replacement for it — `GET /api/todos`'s `text` is
            // this string, and stripping the parenthetical out of it would
            // silently rewrite that route's output for every smart_todo
            // annotation in the repo.
            text: cap_chars(after.trim(), KEYWORD_TEXT_CAP),
            keyword: kw.to_string(),
            fields: parse_fields(after),
        })
    }
}

/// Longest-first, then alphabetical — a total order, so the effective set
/// (and therefore every classification derived from it) is deterministic
/// regardless of the order an operator typed the list in.
fn sort_longest_first(keywords: &mut [String]) {
    keywords.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
}

/// One keyword hit on one comment line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeywordHit {
    pub keyword: String,
    /// Trailing text after the keyword and (when present) its smart_todo
    /// parenthetical, trimmed and capped — `GET /api/todos`'s `text`.
    pub text: String,
    /// `Some` only for a `KEYWORD(key: value, …)` parenthetical.
    pub fields: Option<SmartTodoFields>,
}

/// The parsed `TODO(on: <predicate>, to: '<who>')` bag.
///
/// The bag is OPEN: every `key: value` pair the parenthetical carries is
/// kept in `raw`, so `by:` (or any other key a future smart_todo release
/// adds) round-trips without a grammar change. Only `on:` is INTERPRETED,
/// and only far enough to answer "is this past its date" — the other four
/// predicate shapes the gem supports (`issue_close`, `pull_request_close`,
/// `gem_bump`, `gem_release`) are recorded by NAME and never evaluated:
/// resolving them means a network call, which this daemon does not make.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmartTodoFields {
    /// Every `key: value` pair, value with one layer of matching quotes
    /// stripped. Ordered (BTreeMap) so the stored JSON is deterministic.
    pub raw: BTreeMap<String, String>,
    /// `"date"` | `"issue_close"` | `"pull_request_close"` | `"gem_bump"`
    /// | `"gem_release"` | `"other"` — the shape of `on:`, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_kind: Option<String>,
    /// `YYYY-MM-DD`, only for `on: date('YYYY-MM-DD')`. The ONE field the
    /// drift oracle can evaluate without leaving the box.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_date: Option<String>,
    /// `to:`'s value, quotes stripped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
}

/// Parse a `KEYWORD(key: value, …)` parenthetical into typed fields.
/// `None` for anything that is not one — the trailing text is unaffected
/// either way.
fn parse_fields(after: &str) -> Option<SmartTodoFields> {
    if !after.starts_with('(') {
        return None;
    }
    let Some(close) = matching_paren(after) else {
        // An unbalanced `(` is not a smart_todo — it is ordinary prose
        // that happens to start with a bracket. Never guess a span.
        return None;
    };
    let inner = &after[1..close];
    let pairs = split_top_level(inner);
    let mut raw = BTreeMap::new();
    for item in pairs {
        let Some((k, v)) = item.split_once(':') else {
            continue;
        };
        let key = k.trim();
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }
        raw.insert(key.to_string(), unquote(v.trim()).to_string());
    }
    if raw.is_empty() {
        return None;
    }
    let on = raw.get("on").cloned();
    let on_kind = on.as_deref().map(predicate_kind).map(str::to_string);
    let on_date = on.as_deref().and_then(date_arg);
    let to = raw.get("to").cloned();
    Some(SmartTodoFields {
        raw,
        on_kind,
        on_date,
        to,
    })
}

/// The smart_todo predicate NAME, or `"other"` for anything this grammar
/// does not recognise (never a guess, never an error).
fn predicate_kind(on: &str) -> &'static str {
    let head = on.split('(').next().unwrap_or("").trim();
    match head {
        "date" => "date",
        "issue_close" => "issue_close",
        "pull_request_close" => "pull_request_close",
        "gem_bump" => "gem_bump",
        "gem_release" => "gem_release",
        _ => "other",
    }
}

/// `date('2027-09-01')` → `Some("2027-09-01")`. A bare, unquoted
/// `date(2027-09-01)` is accepted too; anything that is not exactly ten
/// chars of `YYYY-MM-DD` is `None` (an un-evaluatable date is `unknown`,
/// never a guessed one).
fn date_arg(on: &str) -> Option<String> {
    let rest = on.strip_prefix("date")?.trim_start();
    let close = matching_paren(rest)?;
    let arg = unquote(rest[1..close].trim());
    let bytes = arg.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    if !bytes
        .iter()
        .enumerate()
        .all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit())
    {
        return None;
    }
    Some(arg.to_string())
}

/// Byte offset of the `)` matching the `(` at offset 0 of `s`, ignoring
/// brackets inside single/double-quoted runs.
fn matching_paren(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    if bytes.first() != Some(&b'(') {
        return None;
    }
    let mut depth = 0usize;
    let mut quote: Option<u8> = None;
    for (i, &b) in bytes.iter().enumerate() {
        match quote {
            Some(q) => {
                if b == q {
                    quote = None;
                }
            }
            None => match b {
                b'\'' | b'"' => quote = Some(b),
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            },
        }
    }
    None
}

/// Split on commas at paren-depth 0 and outside quotes.
fn split_top_level(inner: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<u8> = None;
    let mut start = 0usize;
    for (i, &b) in inner.as_bytes().iter().enumerate() {
        match quote {
            Some(q) => {
                if b == q {
                    quote = None;
                }
            }
            None => match b {
                b'\'' | b'"' => quote = Some(b),
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth -= 1,
                b',' if depth == 0 => {
                    out.push(&inner[start..i]);
                    start = i + 1;
                }
                _ => {}
            },
        }
    }
    if start <= inner.len() {
        out.push(&inner[start..]);
    }
    out
}

/// Strip ONE layer of matching `'`/`"` quotes, if the whole value is
/// wrapped in them.
fn unquote(v: &str) -> &str {
    let b = v.as_bytes();
    if b.len() >= 2 && (b[0] == b'\'' || b[0] == b'"') && b[b.len() - 1] == b[0] {
        &v[1..v.len() - 1]
    } else {
        v
    }
}

/// First occurrence of `needle` in `line` with ASCII word boundaries on
/// both sides (`TODOS` and `myTODO` never match `TODO`).
fn word_bounded_find(line: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    let bytes = line.as_bytes();
    let mut start = 0usize;
    while start + needle.len() <= line.len() {
        let rel = line[start..].find(needle)?;
        let abs = start + rel;
        let before_ok = abs == 0 || !is_word_byte(bytes[abs - 1]);
        let after = abs + needle.len();
        let after_ok = after >= bytes.len() || !is_word_byte(bytes[after]);
        if before_ok && after_ok {
            return Some(abs);
        }
        start = abs + 1;
    }
    None
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Char-safe truncation (never splits a UTF-8 scalar).
pub fn cap_chars(s: &str, cap: usize) -> String {
    if s.chars().count() <= cap {
        s.to_string()
    } else {
        s.chars().take(cap).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_set_is_rubocops_six_plus_the_two_legacy_markers() {
        let set = KeywordSet::defaults();
        assert_eq!(set.source, "default");
        let mut got: Vec<&str> = set.keywords.iter().map(String::as_str).collect();
        got.sort_unstable();
        let mut want: Vec<&str> = RUBOCOP_KEYWORDS
            .iter()
            .chain(LEGACY_EXTRA_KEYWORDS.iter())
            .copied()
            .collect();
        want.sort_unstable();
        assert_eq!(got, want);
    }

    #[test]
    fn every_todo_family_member_is_in_the_default_set() {
        // The `GET /api/todos` view filters to TODO_FAMILY; if the default
        // set stopped covering it, that route would silently lose rows.
        let set = KeywordSet::defaults();
        for kw in TODO_FAMILY {
            assert!(
                set.keywords.iter().any(|k| k == kw),
                "{kw} is in TODO_FAMILY but not the default keyword set"
            );
        }
    }

    #[test]
    fn config_override_replaces_the_default_set_it_does_not_extend_it() {
        let set = KeywordSet::from_config(&["TODO".to_string(), "DEBT".to_string()]);
        assert_eq!(set.source, "config");
        assert_eq!(set.keywords, vec!["DEBT".to_string(), "TODO".to_string()]);
        assert!(set.find("# FIXME: gone").is_none());
        assert_eq!(set.find("# DEBT: kept").unwrap().keyword, "DEBT");
    }

    #[test]
    fn an_all_blank_override_falls_back_to_the_defaults() {
        let set = KeywordSet::from_config(&["".to_string(), "   ".to_string()]);
        assert_eq!(set.source, "default");
        assert_eq!(set.keywords.len(), 8);
    }

    #[test]
    fn keyword_match_is_word_bounded_and_case_sensitive() {
        let set = KeywordSet::defaults();
        assert!(set.find("// TODOS more").is_none());
        assert!(set.find("// myTODO").is_none());
        assert!(set.find("// todo lowercase").is_none());
        assert_eq!(set.find("// TODO real").unwrap().keyword, "TODO");
    }

    #[test]
    fn trailing_text_matches_the_legacy_todo_index_byte_for_byte() {
        // `extract_todos` captured everything after the marker, trimmed.
        let set = KeywordSet::defaults();
        assert_eq!(
            set.find("// TODO: wire up the sink").unwrap().text,
            ": wire up the sink"
        );
    }

    #[test]
    fn a_two_marker_line_reports_the_leftmost_keyword() {
        // The documented behaviour change vs `find_todo_marker`, which
        // returned whichever marker came first in its hardcoded array.
        let set = KeywordSet::defaults();
        assert_eq!(
            set.find("// TODO: drop this FIXME shim").unwrap().keyword,
            "TODO"
        );
        assert_eq!(
            set.find("// FIXME: this TODO is older").unwrap().keyword,
            "FIXME"
        );
    }

    #[test]
    fn smart_todo_date_and_owner_are_parsed_into_fields() {
        let set = KeywordSet::defaults();
        let hit = set
            .find("# TODO(on: date('2027-09-01'), to: 'owner@example.com') drop the shim")
            .unwrap();
        let f = hit.fields.unwrap();
        assert_eq!(f.on_kind.as_deref(), Some("date"));
        assert_eq!(f.on_date.as_deref(), Some("2027-09-01"));
        assert_eq!(f.to.as_deref(), Some("owner@example.com"));
        // The parenthetical stays IN the text — `fields` is a parsed view
        // of it, not a replacement, so `GET /api/todos`'s `text` for a
        // smart_todo annotation is byte-identical to the pre-comments/1
        // index's.
        assert_eq!(
            hit.text,
            "(on: date('2027-09-01'), to: 'owner@example.com') drop the shim"
        );
    }

    #[test]
    fn the_other_four_predicates_are_named_but_never_evaluated() {
        let set = KeywordSet::defaults();
        for (src, kind) in [
            (
                "# TODO(on: issue_close('o', 'r', '12'), to: 'x')",
                "issue_close",
            ),
            (
                "# TODO(on: pull_request_close('o', 'r', '9'), to: 'x')",
                "pull_request_close",
            ),
            (
                "# TODO(on: gem_bump('rails', '>= 8.0'), to: 'x')",
                "gem_bump",
            ),
            ("# TODO(on: gem_release('rack'), to: 'x')", "gem_release"),
            ("# TODO(on: full_moon(), to: 'x')", "other"),
        ] {
            let f = set.find(src).unwrap().fields.unwrap();
            assert_eq!(f.on_kind.as_deref(), Some(kind), "{src}");
            assert!(f.on_date.is_none(), "{src}: only date() yields a date");
        }
    }

    #[test]
    fn an_unknown_key_rides_the_open_bag_including_by() {
        let set = KeywordSet::defaults();
        let f = set
            .find("# TODO(on: date('2030-01-02'), to: 'a', by: 'b', why: 'c')")
            .unwrap()
            .fields
            .unwrap();
        assert_eq!(f.raw.get("by").map(String::as_str), Some("b"));
        assert_eq!(f.raw.get("why").map(String::as_str), Some("c"));
    }

    #[test]
    fn a_malformed_parenthetical_is_prose_not_a_guessed_span() {
        let set = KeywordSet::defaults();
        let hit = set
            .find("# TODO(on: date('2027-09-01') unterminated")
            .unwrap();
        assert!(hit.fields.is_none());
        assert_eq!(hit.text, "(on: date('2027-09-01') unterminated");
        // A parenthetical with no `key: value` pair at all is not a bag.
        let plain = set.find("# TODO(see the ticket) fix it").unwrap();
        assert!(plain.fields.is_none());
    }

    #[test]
    fn a_bad_date_is_unknown_never_a_guess() {
        let set = KeywordSet::defaults();
        for src in [
            "# TODO(on: date('2027-9-1'), to: 'x')",
            "# TODO(on: date('soon'), to: 'x')",
            "# TODO(on: date(), to: 'x')",
        ] {
            let f = set.find(src).unwrap().fields.unwrap();
            assert_eq!(f.on_kind.as_deref(), Some("date"), "{src}");
            assert!(f.on_date.is_none(), "{src}");
        }
    }

    #[test]
    fn keyword_text_is_capped_without_splitting_a_scalar() {
        let set = KeywordSet::defaults();
        let long = format!("# TODO: {}", "é".repeat(KEYWORD_TEXT_CAP + 50));
        let hit = set.find(&long).unwrap();
        assert_eq!(hit.text.chars().count(), KEYWORD_TEXT_CAP);
    }
}
