//! V71-D1 (design D3) — **the ONE matcher**. Every name-shaped ranking
//! decision in this crate goes through this module: the files lane, the
//! symbols lane, and (via the wire's match indices) the SPA's own list
//! filters. There is deliberately no second implementation of "how does a
//! typed needle rank against a name" anywhere else — the v7.0 defect class
//! was a surface that looked declared but was wired to nothing, and two
//! matchers with two rankings is the same class of lie, one layer down
//! (kb-code shipped exactly that: nucleo server-side, `speedSearch.ts`
//! client-side, only one of them highlighted — recon/search.md §4 gap 17).
//!
//! # Hard tiers above everything learned
//!
//! [`MatchTier`] is a STRUCTURAL ordering key, not a weight:
//!
//! | tier | meaning |
//! |---|---|
//! | [`MatchTier::Exact`] | the candidate's NAME equals the needle, case-insensitively (the file's last path segment; a symbol's own name, or its `Container::name` form) |
//! | [`MatchTier::Prefix`] | the name STARTS WITH the needle |
//! | [`MatchTier::Fuzzy`] | nucleo matched somewhere in the haystack |
//!
//! Callers sort by `(tier, then score desc, then their own deterministic
//! tie-breaks)`, and every learned/provenance multiplier (frecency, …)
//! applies WITHIN a tier. That is the JetBrains Search-Everywhere failure
//! mode designed out rather than weighted around: `Demo.spec.tsx` can never
//! lose to `DemoFile.spec.tsx` because a learned signal outgrew a fuzzy
//! score, since the exact tier is above the arithmetic entirely
//! (research/search-understands-codebase.md §1.1, §3.2 "Tier 0"). Blackbird's
//! partial-match penalty is the same rule read the other way round —
//! `thread` ranks above `thread_id` above `pthread_getname_np`.
//!
//! # Match indices are UTF-16 offsets
//!
//! nucleo reports match positions as CHAR indices into its `Utf32Str`
//! haystack. The only consumer of those positions is JavaScript, whose
//! string offsets are UTF-16 code units — so [`NameMatch::ranges`] is
//! converted here, ONCE, on the way out (`ch.len_utf16()` accumulation, with
//! an ASCII fast path), rather than making every client re-derive a mapping
//! the server already had in hand. Ranges are sorted, de-duplicated and
//! merged into half-open `[start, end)` pairs, so a client can feed them
//! straight into a highlight pass.
//!
//! # Dual identifier tokenisation + rarity
//!
//! [`identifier_atoms`] splits an identifier BOTH ways at once — the whole
//! token AND its camel/snake/kebab sub-tokens — which is the 2026 BM25-over-
//! code tokenisation result (a Go/Java corpus wants the humps split, a
//! Python one wants the whole word; indexing both is what serves both), and
//! [`rarity_weight`] is the correction for IDF's famously flat tail: a
//! `df = 1` hapax like `handleWebSocketUpgrade` and a `df = 50` identifier
//! otherwise differ by a small additive constant instead of an order of
//! magnitude. Both are PURE functions of their arguments — no store, no
//! config, no clock — and the lexical lane (`super::text`) is their only
//! caller today.

use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use nucleo::{Config, Matcher, Utf32Str};

/// The structural ordering tier of one match — see the module doc. Lower
/// discriminant sorts FIRST; `#[derive(PartialOrd, Ord)]` over the
/// declaration order is what callers sort on, so the variants' ORDER here is
/// load-bearing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MatchTier {
    Exact,
    Prefix,
    Fuzzy,
}

impl MatchTier {
    pub fn as_str(self) -> &'static str {
        match self {
            MatchTier::Exact => "exact",
            MatchTier::Prefix => "prefix",
            MatchTier::Fuzzy => "fuzzy",
        }
    }
}

/// What a haystack IS, which decides both nucleo's delimiter set and where
/// the [`MatchTier`] comparison looks for "the name".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HaystackKind {
    /// A repo-relative path — the name is the last `/`-separated segment.
    Path,
    /// A symbol's `Container::name` (or bare `name`) — the name is the
    /// segment after the last `::`, and the WHOLE haystack also counts as a
    /// name (so `@GitRepo::open` can be an exact hit too).
    Symbol,
}

/// One scored match. `ranges` are UTF-16 `[start, end)` offsets into the
/// haystack the caller passed — see the module doc.
#[derive(Debug, Clone, PartialEq)]
pub struct NameMatch {
    pub score: u32,
    pub tier: MatchTier,
    pub ranges: Vec<[u32; 2]>,
}

/// A parsed needle plus nucleo's reusable scratch state. Build ONE per
/// search call and score every candidate through it — `Pattern::parse` and
/// `Matcher::new` both allocate, and a per-candidate rebuild is how a
/// 26,000-path repo turns an instant lane into a slow one.
pub struct NameMatcher {
    pattern: Pattern,
    matcher: Matcher,
    needle_lower: String,
    /// Scratch for `Utf32Str::new` — reused across candidates.
    buf: Vec<char>,
    /// Scratch for `Pattern::indices` — reused across candidates.
    indices: Vec<u32>,
}

impl NameMatcher {
    /// Parse `needle` once for the whole search. `kind` selects nucleo's
    /// config: [`HaystackKind::Path`] uses `Config::DEFAULT.match_paths()`
    /// (boundary bonus narrowed to the path separator, fzf-style file-picker
    /// intuition), [`HaystackKind::Symbol`] plain `Config::DEFAULT` (whose
    /// delimiter set already includes `:`, exactly the boundary a
    /// `Container::name` haystack wants a bonus after — `.match_paths()`
    /// would NARROW the set and drop it).
    pub fn new(needle: &str, kind: HaystackKind) -> Self {
        let config = match kind {
            HaystackKind::Path => Config::DEFAULT.match_paths(),
            HaystackKind::Symbol => Config::DEFAULT,
        };
        Self {
            pattern: Pattern::parse(needle, CaseMatching::Ignore, Normalization::Smart),
            matcher: Matcher::new(config),
            needle_lower: needle.trim().to_lowercase(),
            buf: Vec::new(),
            indices: Vec::new(),
        }
    }

    /// Score `haystack`, returning `None` when nucleo finds no match at all.
    /// `kind` must be the same one [`Self::new`] was given (it decides where
    /// the tier comparison looks for "the name" — see [`HaystackKind`]).
    pub fn score(&mut self, haystack: &str, kind: HaystackKind) -> Option<NameMatch> {
        self.buf.clear();
        self.indices.clear();
        let hay = Utf32Str::new(haystack, &mut self.buf);
        let score = self
            .pattern
            .indices(hay, &mut self.matcher, &mut self.indices)?;
        let ranges = char_indices_to_utf16_ranges(haystack, &mut self.indices);
        Some(NameMatch {
            score,
            tier: tier_of(&self.needle_lower, haystack, kind),
            ranges,
        })
    }
}

/// The name portion a [`MatchTier`] comparison runs against — see
/// [`HaystackKind`]. Returns the haystack itself when it carries no
/// separator.
fn name_of(haystack: &str, kind: HaystackKind) -> &str {
    match kind {
        HaystackKind::Path => haystack.rsplit('/').next().unwrap_or(haystack),
        HaystackKind::Symbol => match haystack.rfind("::") {
            Some(i) => &haystack[i + 2..],
            None => haystack,
        },
    }
}

/// Classify one candidate — see the module doc's tier table. `needle_lower`
/// must already be lowercased and trimmed (the caller lowercases ONCE per
/// search, never once per candidate).
fn tier_of(needle_lower: &str, haystack: &str, kind: HaystackKind) -> MatchTier {
    if needle_lower.is_empty() {
        return MatchTier::Fuzzy;
    }
    let name = name_of(haystack, kind).to_lowercase();
    if name == needle_lower {
        return MatchTier::Exact;
    }
    // A symbol's fully-qualified form is a name in its own right: an
    // `@GitRepo::open` query naming a symbol exactly should not be demoted
    // to Fuzzy just because the tier comparison only looked at `open`.
    if kind == HaystackKind::Symbol {
        let full = haystack.to_lowercase();
        if full == needle_lower {
            return MatchTier::Exact;
        }
        if full.starts_with(needle_lower) {
            return MatchTier::Prefix;
        }
    }
    if name.starts_with(needle_lower) {
        return MatchTier::Prefix;
    }
    MatchTier::Fuzzy
}

/// Sort/dedup nucleo's raw char indices (it appends per pattern ATOM and
/// documents that the result is neither sorted nor deduplicated) and fold
/// them into merged, half-open UTF-16 ranges — see the module doc.
/// `indices` is drained-in-place scratch, not read afterwards.
fn char_indices_to_utf16_ranges(haystack: &str, indices: &mut Vec<u32>) -> Vec<[u32; 2]> {
    indices.sort_unstable();
    indices.dedup();
    if indices.is_empty() {
        return Vec::new();
    }
    // Fast path: for an all-ASCII haystack a char index IS the UTF-16
    // offset, so the per-char walk below is pure cost.
    let ascii = haystack.is_ascii();
    let mut utf16_of: Vec<u32> = Vec::new();
    if !ascii {
        utf16_of.reserve(haystack.chars().count() + 1);
        let mut acc = 0u32;
        for ch in haystack.chars() {
            utf16_of.push(acc);
            acc += ch.len_utf16() as u32;
        }
        utf16_of.push(acc);
    }
    let at = |char_idx: u32| -> Option<(u32, u32)> {
        if ascii {
            let n = haystack.len() as u32;
            (char_idx < n).then_some((char_idx, char_idx + 1))
        } else {
            let i = char_idx as usize;
            (i + 1 < utf16_of.len()).then(|| (utf16_of[i], utf16_of[i + 1]))
        }
    };

    let mut out: Vec<[u32; 2]> = Vec::new();
    for &idx in indices.iter() {
        let Some((start, end)) = at(idx) else {
            continue;
        };
        match out.last_mut() {
            Some(last) if last[1] == start => last[1] = end,
            _ => out.push([start, end]),
        }
    }
    out
}

/// Dual identifier tokenisation — see the module doc. Splits `text` on
/// every non-alphanumeric byte, then splits each token on camel humps and
/// letter↔digit boundaries, and returns BOTH the whole token and its
/// sub-tokens, lowercased, in first-seen order with duplicates removed.
/// Sub-tokens shorter than [`MIN_ATOM_LEN`] are dropped (a `get_x` should
/// not contribute an `x` atom that matches half the corpus) — the WHOLE
/// token is always kept regardless of length, since a caller searching for
/// `id` means it.
pub fn identifier_atoms(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let push = |atom: String, out: &mut Vec<String>| {
        if !atom.is_empty() && !out.iter().any(|a| *a == atom) {
            out.push(atom);
        }
    };
    // `_` is PART of an identifier, not a delimiter, at this level — that is
    // what keeps `order_total` in the atom set beside `order` and `total`
    // (the "whole identifier AND its sub-tokens" half of dual tokenisation;
    // splitting on `_` here would silently drop every snake_case whole
    // token). `split_humps` is where `_` becomes a boundary.
    for token in text.split(|c: char| !c.is_alphanumeric() && c != '_') {
        if token.is_empty() {
            continue;
        }
        let whole = token.to_lowercase();
        push(whole.clone(), &mut out);
        let subs = split_humps(token);
        if subs.len() > 1 {
            for sub in subs {
                if sub.chars().count() >= MIN_ATOM_LEN {
                    push(sub.to_lowercase(), &mut out);
                }
            }
        }
    }
    out
}

/// Shortest camel/snake SUB-token [`identifier_atoms`] will emit beside the
/// whole identifier — see that fn's doc.
pub const MIN_ATOM_LEN: usize = 2;

/// Split one whole identifier into its sub-tokens: on `_` (snake_case), on
/// camel humps (`fooBar`; `HTTPServer` → `HTTP` + `Server`) and on
/// letter↔digit boundaries (`utf8Str` → `utf`, `8`, `Str`).
fn split_humps(token: &str) -> Vec<&str> {
    if token.contains('_') {
        return token
            .split('_')
            .filter(|p| !p.is_empty())
            .flat_map(split_humps)
            .collect();
    }
    let mut out = Vec::new();
    let chars: Vec<(usize, char)> = token.char_indices().collect();
    let mut start = 0usize;
    for w in 1..chars.len() {
        let (i, cur) = chars[w];
        let (_, prev) = chars[w - 1];
        let boundary = (cur.is_uppercase() && !prev.is_uppercase())
            || (cur.is_numeric() != prev.is_numeric())
            // `HTTPServer` — the hump is BEFORE the last capital of a run.
            || (cur.is_lowercase()
                && prev.is_uppercase()
                && w >= 2
                && chars[w - 2].1.is_uppercase());
        if boundary {
            let end = if cur.is_lowercase() && prev.is_uppercase() {
                chars[w - 1].0
            } else {
                i
            };
            if end > start {
                out.push(&token[start..end]);
                start = end;
            }
        }
    }
    if start < token.len() {
        out.push(&token[start..]);
    }
    out
}

/// Rarity weight for an atom occurring in `df` of `n` candidates — the flat-
/// IDF-tail correction from the module doc. `ln(1 + n / (1 + df))`: strictly
/// positive (so a term every candidate carries still contributes, and the
/// ordering degrades to the occurrence-count tie-break rather than to zero),
/// monotonically DECREASING in `df`, and never a divide-by-zero. `df = 0`
/// (an atom nothing carries) and `n = 0` (an empty candidate set) are both
/// defined rather than special-cased at every call site.
pub fn rarity_weight(df: usize, n: usize) -> f64 {
    (1.0 + n as f64 / (1.0 + df as f64)).ln()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(needle: &str, hay: &str, kind: HaystackKind) -> Option<NameMatch> {
        NameMatcher::new(needle, kind).score(hay, kind)
    }

    #[test]
    fn exact_path_segment_is_the_exact_tier() {
        let hit = m(
            "config.rs",
            "crates/kb-code-server/src/config.rs",
            HaystackKind::Path,
        )
        .unwrap();
        assert_eq!(hit.tier, MatchTier::Exact);
    }

    #[test]
    fn a_longer_name_containing_the_needle_is_never_exact() {
        // Blackbird's partial-match penalty, expressed as a tier: the exact
        // file can never lose to a longer one carrying the same needle.
        let exact = m("demo.spec.tsx", "web/demo.spec.tsx", HaystackKind::Path).unwrap();
        let longer = m("demo.spec.tsx", "web/DemoFile.spec.tsx", HaystackKind::Path);
        assert_eq!(exact.tier, MatchTier::Exact);
        if let Some(longer) = longer {
            assert_eq!(longer.tier, MatchTier::Fuzzy);
            assert!(exact.tier < longer.tier, "exact must sort before fuzzy");
        }
    }

    #[test]
    fn prefix_tier_sits_between_exact_and_fuzzy() {
        let p = m("thread", "src/thread_id.rs", HaystackKind::Path).unwrap();
        assert_eq!(p.tier, MatchTier::Prefix);
        let f = m("thread", "src/pthread_getname.rs", HaystackKind::Path).unwrap();
        assert_eq!(f.tier, MatchTier::Fuzzy);
        assert!(MatchTier::Exact < MatchTier::Prefix);
        assert!(MatchTier::Prefix < MatchTier::Fuzzy);
    }

    #[test]
    fn symbol_tier_reads_both_the_bare_name_and_the_qualified_form() {
        assert_eq!(
            m("open", "GitRepo::open", HaystackKind::Symbol)
                .unwrap()
                .tier,
            MatchTier::Exact
        );
        assert_eq!(
            m("gitrepo::open", "GitRepo::open", HaystackKind::Symbol)
                .unwrap()
                .tier,
            MatchTier::Exact
        );
        assert_eq!(
            m("gitrepo", "GitRepo::open", HaystackKind::Symbol)
                .unwrap()
                .tier,
            MatchTier::Prefix
        );
    }

    #[test]
    fn match_ranges_are_merged_and_ascii_offsets_are_byte_offsets() {
        let hit = m("cfg", "src/config.rs", HaystackKind::Path).unwrap();
        assert!(!hit.ranges.is_empty());
        for r in &hit.ranges {
            assert!(r[0] < r[1], "half-open, non-empty: {r:?}");
        }
        // Sorted and non-overlapping.
        for w in hit.ranges.windows(2) {
            assert!(w[0][1] <= w[1][0], "overlapping ranges: {:?}", hit.ranges);
        }
        // A contiguous run merges into ONE range.
        let contig = m("config", "src/config.rs", HaystackKind::Path).unwrap();
        assert_eq!(contig.ranges, vec![[4, 10]]);
    }

    #[test]
    fn match_ranges_are_utf16_offsets_not_char_indices() {
        // "🦀" is ONE char but TWO UTF-16 code units — a JS consumer
        // slicing on char indices would highlight the wrong columns.
        let hay = "src/🦀/config.rs";
        let hit = m("config", hay, HaystackKind::Path).unwrap();
        let utf16: Vec<u16> = hay.encode_utf16().collect();
        let r = hit.ranges[0];
        let sliced = String::from_utf16(&utf16[r[0] as usize..r[1] as usize]).unwrap();
        assert_eq!(sliced, "config");
    }

    #[test]
    fn identifier_atoms_keeps_the_whole_token_and_its_sub_tokens() {
        assert_eq!(
            identifier_atoms("handleWebSocketUpgrade"),
            vec![
                "handlewebsocketupgrade",
                "handle",
                "web",
                "socket",
                "upgrade"
            ]
        );
        assert_eq!(
            identifier_atoms("order_total"),
            vec!["order_total", "order", "total"]
        );
        // `HTTPServer` — the hump is before the LAST capital of the run.
        assert_eq!(
            identifier_atoms("HTTPServer"),
            vec!["httpserver", "http", "server"]
        );
    }

    #[test]
    fn identifier_atoms_splits_letter_digit_boundaries_and_dedups() {
        // `8` is below MIN_ATOM_LEN and drops out; `utf` and `str` stay.
        assert_eq!(identifier_atoms("utf8Str"), vec!["utf8str", "utf", "str"]);
        // The same atom twice yields one entry, first-seen order preserved.
        assert_eq!(identifier_atoms("order order"), vec!["order"]);
    }

    #[test]
    fn identifier_atoms_drops_one_char_sub_tokens_but_never_a_whole_token() {
        // `get_x` → the sub-token `x` is below MIN_ATOM_LEN and dropped …
        assert_eq!(identifier_atoms("get_x"), vec!["get_x", "get"]);
        // … and a path is split on its non-identifier punctuation.
        assert_eq!(
            identifier_atoms("app/models/order.rb"),
            vec!["app", "models", "order", "rb"]
        );
        // … but a caller literally searching for `x` keeps it.
        assert_eq!(identifier_atoms("x"), vec!["x"]);
    }

    #[test]
    fn rarity_weight_is_positive_and_decreasing_in_df() {
        let hapax = rarity_weight(1, 50);
        let common = rarity_weight(50, 50);
        assert!(hapax > common, "{hapax} !> {common}");
        assert!(common > 0.0, "a term in every candidate still contributes");
        // Defined at the edges rather than special-cased per call site.
        assert!(rarity_weight(0, 0).is_finite());
        assert!(rarity_weight(0, 100) > rarity_weight(1, 100));
    }

    #[test]
    fn an_empty_needle_matches_everything_at_the_fuzzy_tier() {
        let hit = m("", "src/lib.rs", HaystackKind::Path).unwrap();
        assert_eq!(hit.tier, MatchTier::Fuzzy);
        assert!(hit.ranges.is_empty());
    }
}
