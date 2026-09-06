//! DCB W1.C — the resolution engine.
//!
//! [`resolve_lens`] is the ONE entry point (R8): the HTTP handlers in
//! `super::wire` are thin wrappers around it, and W3.A's background sync
//! calls it IN-PROCESS rather than HTTP-ing this daemon, so the `[doclens]
//! kbs` gate, the segment validation and the deadline are inherited rather
//! than re-implemented three times.
//!
//! Everything below the `spawn_blocking` hop is deterministic and LLM-free:
//! ONE `Store::list_files` and ONE `Store::symbols_named_many` per (repo,
//! request), a per-request `(repo, path)` file-read memo, and pure predicates
//! over the bytes those reads produced. The fuzzy `search::{files,symbols}`
//! lanes are never called — they carry frecency (non-deterministic across two
//! otherwise-identical requests) and no cardinality.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use serde::Serialize;

use super::remap::{
    path_exists_at_rev, read_text_at_rev, LineMap, RemapState, RevRemap, SpanRemap,
};
use super::wire::{
    CodeLensOut, CodeRevOut, GroupOut, LensCounts, PathLensOut, ReaderTarget, RefOut, RepoOut,
    RevRemapOut, ScorecardOut, ScorecardRepoOut, WhenWrittenOut,
};
use super::{
    doc_href, gate, gate_kb_only, validate_kb_segment, AMBIGUITY_INLINE_MAX, CONFIRM_WINDOW_LINES,
    DRIFT_WINDOW_LINES, LENS_FILE_READ_CAP, LENS_NOTE, MAX_CONFIRM_TOKENS, MAX_SYMBOL_HITS,
    MIN_TOKEN_LEN, PATH_LENS_NOTE, PATH_LENS_SCHEMA, SCHEMA, SCORECARD_FANOUT_CAP,
    SCORECARD_SCHEMA, TOKEN_STOPLIST,
};
use crate::extract::Symbol;
use crate::git::GitRepo;
use crate::join::kb_client::{CodeRefRow, CodeRefsDoc, KbClientError};
use crate::routes::{find_repo, ApiError};
use crate::state::SharedState;
use crate::store::{FileRow, Store, StoreBlocking};

// --- vocabulary ------------------------------------------------------------

/// doc-lens's own path vocabulary. `null` (an `Option::None` on the wire) is
/// the FIFTH case — a ref carrying no `path_hint` at all (D5) — deliberately
/// kept out of the enum so the four values stay cardinalities.
///
/// **`indexing` is never a `path_state`** — it is a per-REPO scorecard state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PathState {
    Present,
    Ambiguous,
    Absent,
    External,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LineState {
    Confirmed,
    Drifted,
    Unverifiable,
    Absent,
}

impl LineState {
    /// The wire spelling, as a `&'static str`. DCB W3.A persists
    /// `line_state` as TEXT in `doc_refs` and echoes it back on
    /// `doc-refs/1`, and a `serde_json::to_value` round trip to read one
    /// enum variant would be an odd way to spell a constant.
    /// `line_state_strings_match_the_wire_exactly` (`doclens::sync`'s tests)
    /// pins this against the `Serialize` derive so the two can never drift.
    pub fn as_str(self) -> &'static str {
        match self {
            LineState::Confirmed => "confirmed",
            LineState::Drifted => "drifted",
            LineState::Unverifiable => "unverifiable",
            LineState::Absent => "absent",
        }
    }

    /// Rollup rank for a multi-span ref: the WEAKEST span wins (§18.2).
    fn rank(self) -> u8 {
        match self {
            LineState::Confirmed => 3,
            LineState::Drifted => 2,
            LineState::Unverifiable => 1,
            LineState::Absent => 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LineEvidence {
    /// W2.A — a `kb-code-rev`-anchored `git diff` remap. Never emitted in
    /// W1.C; the variant exists so consumers (and the CLI renderer) key on
    /// `line_evidence` from day one rather than on `line_state` alone.
    RevRemap,
    ContextToken,
    None,
}

/// doc-lens's OWN symbol vocabulary (amendment 6). It deliberately does not
/// borrow `crate::resolve`'s `exact`/`likely`/`candidate` trust classes: a
/// doc reference carries strictly less evidence than a repo-wide-unique name
/// match, and `doclens_never_emits_a_resolve_trust_class` pins that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolState {
    HitUnique,
    HitContainerMatched,
    HitAmbiguous,
    NoSymbol,
}

/// A GitHub issue citation, re-assembled from the fields the producer reuses
/// rather than adding columns for: `path_hint = "<owner>/<repo>"`,
/// `line_start` = the issue NUMBER. doc-lens is the ONLY place that overload
/// is decoded — every consumer reads this struct (R11). The href is REBUILT
/// from the parts, never taken from `raw` (which may be a
/// `#issuecomment-…`-suffixed variant depending on document order).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IssueRef {
    pub owner: String,
    pub repo: String,
    pub number: u32,
    pub href: String,
}

/// The ONE place the fuzzy lane appears in the whole design — as a LINK, never
/// as a verdict. Clients build `/search?q={q}&repo={repo}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SearchLink {
    pub q: String,
    pub repo: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SymbolHit {
    pub path: String,
    pub line_start: u32,
    pub line_end: u32,
    pub kind: String,
    pub container: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PathResolution {
    /// `None` when the ref carries no `path_hint` (D5) or is an issue (R11).
    pub state: Option<PathState>,
    pub resolved: Option<String>,
    pub candidate_count: usize,
    /// `<= AMBIGUITY_INLINE_MAX`, else empty (the count is still exact).
    pub candidates: Vec<String>,
    pub search: Option<SearchLink>,
    pub issue: Option<IssueRef>,
    pub note: Option<String>,
}

impl PathResolution {
    fn empty() -> Self {
        Self {
            state: None,
            resolved: None,
            candidate_count: 0,
            candidates: Vec::new(),
            search: None,
            issue: None,
            note: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LineOutcome {
    pub state: LineState,
    pub evidence: LineEvidence,
    pub token: Option<String>,
    pub token_line: Option<u32>,
    pub resolved_line: Option<u32>,
    /// W2.A ranges (E6) — the mapped END of a `path:5-19` span. `None` on
    /// every token-pass outcome (the token predicate proves a point, never a
    /// span) and on a single-line citation.
    pub resolved_line_end: Option<u32>,
    pub line_hint_delta: Option<i64>,
    /// Always reported so a client can say "the file only has N lines". `0`
    /// means no file was read at all — the deadline cut and the `absent`/
    /// `invalid_line_hint` paths, none of which touch a file. A `rev_remap`
    /// outcome (§4 arm 1) is NOT exempt from this: DCB-W2.A.R fix 2 bound-
    /// checks every mapped line/end against the working tree's real length
    /// via one memoised read per path (`line_half`'s `read_lazily`), so a
    /// SHIPPED `rev_remap` confirmation always carries the real count too —
    /// `resolved_line <= file_lines` is the invariant that read exists to
    /// enforce, not an incidental side effect.
    pub file_lines: u32,
    pub reason: Option<&'static str>,
}

impl LineOutcome {
    fn absent(reason: &'static str) -> Self {
        Self {
            state: LineState::Absent,
            evidence: LineEvidence::None,
            token: None,
            token_line: None,
            resolved_line: None,
            resolved_line_end: None,
            line_hint_delta: None,
            file_lines: 0,
            reason: Some(reason),
        }
    }

    fn unverifiable(reason: &'static str, file_lines: u32) -> Self {
        Self {
            state: LineState::Unverifiable,
            evidence: LineEvidence::None,
            token: None,
            token_line: None,
            resolved_line: None,
            resolved_line_end: None,
            line_hint_delta: None,
            file_lines,
            reason: Some(reason),
        }
    }
}

/// One resolved span of a `path_list` ref (or the single span of a
/// `path_line`/`path_range` ref, in which case `spans.len() == 1`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SpanOutcome {
    pub line_hint: u32,
    pub line_hint_end: Option<u32>,
    pub line_state: LineState,
    pub line_evidence: LineEvidence,
    pub confirm_token: Option<String>,
    pub token_line: Option<u32>,
    pub resolved_line: Option<u32>,
    /// W2.A ranges — always `None` in W1.C.
    pub resolved_line_end: Option<u32>,
    pub line_hint_delta: Option<i64>,
    pub line_reason: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SymbolResolution {
    pub state: SymbolState,
    pub hit_count: usize,
    pub hits: Vec<SymbolHit>,
}

impl SymbolResolution {
    fn none() -> Self {
        Self {
            state: SymbolState::NoSymbol,
            hit_count: 0,
            hits: Vec::new(),
        }
    }
}

// --- RepoSnapshot ----------------------------------------------------------

/// ONE `list_files` read per (repo, request), folded into the two lookup maps
/// path resolution needs. Deliberately NOT `search::files::FileIndex`: that
/// lane is fuzzy + frecency-weighted, its `snapshot()` is private and
/// generation-keyed, and it drops `size` (which the read cap needs).
pub struct RepoSnapshot {
    pub repo_id: i64,
    pub name: String,
    pub root: PathBuf,
    rows: Vec<FileRow>,
    by_path: HashMap<String, usize>,
    by_basename: HashMap<String, Vec<usize>>,
}

impl RepoSnapshot {
    pub fn build(
        store: &Store,
        repo_id: i64,
        name: &str,
        root: &Path,
    ) -> Result<Self, crate::store::StoreError> {
        // `list_files` is already `ORDER BY path`, which is what makes the
        // candidate lists below deterministic without a second sort.
        let rows = store.list_files(repo_id)?;
        Ok(Self::from_rows(repo_id, name, root, rows))
    }

    /// The index-building half, split out so the pure path-resolution tests
    /// can build a snapshot from synthetic rows without a sqlite file.
    /// `rows` MUST already be path-ascending (`list_files`' own ORDER BY) —
    /// that is what makes the candidate lists deterministic.
    pub(crate) fn from_rows(repo_id: i64, name: &str, root: &Path, rows: Vec<FileRow>) -> Self {
        let mut by_path = HashMap::with_capacity(rows.len());
        let mut by_basename: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, r) in rows.iter().enumerate() {
            by_path.insert(r.path.clone(), i);
            let base = basename(&r.path).to_string();
            by_basename.entry(base).or_default().push(i);
        }
        Self {
            repo_id,
            name: name.to_string(),
            root: root.to_path_buf(),
            rows,
            by_path,
            by_basename,
        }
    }

    pub fn exact(&self, path: &str) -> Option<&FileRow> {
        self.by_path.get(path).map(|i| &self.rows[*i])
    }

    /// Every row whose path equals `norm` or ends with `"/" + norm`.
    ///
    /// Implemented as a basename bucket lookup (O(1)) followed by the suffix
    /// test, which makes the bare-basename case (`conversion.rb`) and the
    /// multi-segment-tail case (`products/algolia.rb`) ONE rule — and it is
    /// the anti-fuzzy property that matters: `carts_controller.rb` cannot
    /// match `carts_controller_spec.rb`, because the suffix must be preceded
    /// by a `/` (or be the whole path).
    ///
    /// The result is sorted PATH-ASCENDING here rather than inherited from
    /// `list_files`' own `ORDER BY path`: the candidate list is a rendered,
    /// user-visible artifact on the ambiguous tier, and it must be a property
    /// of the SET, not of whatever order the rows happened to arrive in — the
    /// frecency-invariance golden reverses the row order specifically to
    /// prove that.
    pub fn suffix_candidates(&self, norm: &str) -> Vec<&FileRow> {
        let base = basename(norm);
        let Some(bucket) = self.by_basename.get(base) else {
            return Vec::new();
        };
        let tail = format!("/{norm}");
        let mut hits: Vec<&FileRow> = bucket
            .iter()
            .map(|i| &self.rows[*i])
            .filter(|r| r.path == norm || r.path.ends_with(&tail))
            .collect();
        hits.sort_by(|a, b| a.path.cmp(&b.path));
        hits
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

// --- FileText (the per-request read memo's payload) ------------------------

/// A file's text plus its line offsets, computed ONCE per (repo, path) per
/// request. Line numbers are 1-based everywhere in this module.
pub struct FileText {
    text: String,
    /// `(start, end)` byte offsets per line, newline excluded.
    line_ranges: Vec<(usize, usize)>,
}

impl FileText {
    pub fn new(text: String) -> Self {
        let mut line_ranges = Vec::new();
        let bytes = text.as_bytes();
        let mut start = 0usize;
        for (i, b) in bytes.iter().enumerate() {
            if *b == b'\n' {
                let mut end = i;
                if end > start && bytes[end - 1] == b'\r' {
                    end -= 1;
                }
                line_ranges.push((start, end));
                start = i + 1;
            }
        }
        if start < bytes.len() {
            line_ranges.push((start, bytes.len()));
        }
        Self { text, line_ranges }
    }

    pub fn line_count(&self) -> u32 {
        self.line_ranges.len() as u32
    }

    /// 1-based line access.
    pub fn line(&self, n: u32) -> Option<&str> {
        if n == 0 {
            return None;
        }
        self.line_ranges
            .get((n - 1) as usize)
            .map(|(s, e)| &self.text[*s..*e])
    }
}

/// Case-SENSITIVE, WORD-BOUNDED substring test. Case-sensitive because
/// identifiers are case-significant and the doc quotes them verbatim;
/// word-bounded so `token` does not match inside `user_token`.
pub fn contains_word(hay: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let hb = hay.as_bytes();
    let nb = needle.as_bytes();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut from = 0usize;
    while let Some(rel) = hay[from..].find(needle) {
        let at = from + rel;
        let before_ok = at == 0 || !is_word(hb[at - 1]);
        let after = at + nb.len();
        let after_ok = after >= hb.len() || !is_word(hb[after]);
        if before_ok && after_ok {
            return true;
        }
        // Advance past this match by the WIDTH OF ITS FIRST CHARACTER, not by
        // one byte — `needle` matched byte-for-byte at `at`, so its first
        // char is exactly the char at `hay[at..]`; stepping by a raw `+ 1`
        // can land mid-codepoint (e.g. `contains_word("aède", "ède")`, where
        // the rejected match starts on the 2-byte `è`) and the next
        // `hay[from..]` slice panics on a non-char-boundary index.
        from = at + needle.chars().next().map_or(1, char::len_utf8);
        if from >= hay.len() {
            break;
        }
    }
    false
}

// --- confirm tokens --------------------------------------------------------

/// The confirm-token vector for one ref, in CANONICAL order (the order every
/// tie-break in [`line_state`] refers back to).
///
/// The producer (`coderef/1`'s `context_tokens`) already harvested the
/// code-ish tokens the author marked up, front-loading `symbol_member`, then
/// the last `::` segment of `symbol_container`, then the path stem, then
/// document-order code tokens — so doc-lens re-derives NOTHING and needs no
/// prose stoplist at all. It applies exactly two rules of its own:
///
/// 1. drop a token shorter than `MIN_TOKEN_LEN` after stripping a trailing
///    `!`/`?` (defence in depth — the producer enforces the same floor);
/// 2. DEMOTE (never drop) path-derived tokens to the tail, because a path
///    stem encodes WHERE, not WHAT: `conversion` matches
///    `event_type: 'conversion'` in a file it says nothing about.
///
/// The `TOKEN_STOPLIST` prose scan below survives only as the FALLBACK for a
/// ref whose `context_tokens` is absent or empty (an older row, or a producer
/// shipped without the field).
pub fn confirm_tokens(r: &CodeRefRow) -> Vec<String> {
    let mut toks: Vec<String> = if r.context_tokens.is_empty() {
        fallback_tokens(r)
    } else {
        let mut seen = BTreeSet::new();
        r.context_tokens
            .iter()
            .filter(|t| t.trim_end_matches(['!', '?']).len() >= MIN_TOKEN_LEN)
            .filter(|t| seen.insert((*t).clone()))
            .cloned()
            .collect()
    };
    demote_path_derived(&mut toks, r.path_hint.as_deref());
    toks.truncate(MAX_CONFIRM_TOKENS);
    toks
}

fn fallback_tokens(r: &CodeRefRow) -> Vec<String> {
    let mut head: Vec<String> = Vec::new();
    if let Some(m) = &r.symbol_member {
        head.push(m.clone());
    }
    if let Some(c) = &r.symbol_container {
        head.push(c.clone());
    }

    // Identifier runs `[A-Za-z_][A-Za-z0-9_]*` over the human `context`,
    // hand-rolled (no `regex` dependency in kb-code-server).
    let ctx = r.context.as_deref().unwrap_or("");
    let bytes = ctx.as_bytes();
    let mut scanned: Vec<(String, usize)> = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_alphabetic() || b == b'_' {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let tok = &ctx[start..i];
            if tok.len() >= MIN_TOKEN_LEN
                && !TOKEN_STOPLIST.contains(&tok.to_ascii_lowercase().as_str())
            {
                scanned.push((tok.to_string(), start));
            }
        } else {
            i += 1;
        }
    }
    // Dedup preserving FIRST occurrence (and its offset).
    let mut seen = BTreeSet::new();
    scanned.retain(|(t, _)| seen.insert(t.clone()));
    // Descending length, tie → ascending first-byte offset, tie → lexicographic.
    scanned.sort_by(|a, b| {
        b.0.len()
            .cmp(&a.0.len())
            .then(a.1.cmp(&b.1))
            .then(a.0.cmp(&b.0))
    });

    let mut out = head;
    for (t, _) in scanned {
        if !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

/// Move tokens that merely echo the path to the TAIL, preserving relative
/// order on both sides.
fn demote_path_derived(toks: &mut Vec<String>, path_hint: Option<&str>) {
    let Some(hint) = path_hint else { return };
    let mut derived: BTreeSet<String> = BTreeSet::new();
    for seg in hint.split('/') {
        if seg.is_empty() {
            continue;
        }
        derived.insert(seg.to_ascii_lowercase());
        if let Some(stem) = seg.split('.').next() {
            derived.insert(stem.to_ascii_lowercase());
        }
    }
    let (path_ish, rest): (Vec<String>, Vec<String>) = toks
        .drain(..)
        .partition(|t| derived.contains(&t.to_ascii_lowercase()));
    toks.extend(rest);
    toks.extend(path_ish);
}

// --- line_state ------------------------------------------------------------

/// W2.A §4 **arm 1** — the `kb-code-rev` remap, pure and file-free: THIS
/// function never touches a filesystem, and always returns `file_lines: 0`.
///
/// `None` means "this remap has nothing to say; run the token pass (arm 2)":
/// the cited line was itself edited (`InsideChange`, E4 — the token may still
/// find the moved content), or only ONE endpoint of a range mapped (E6 — a
/// half-mapped span is a lie about the span, so the whole ref falls through).
///
/// `Some` is the STRONGEST evidence class in the vocabulary: git proved the
/// line's identity across the interval, so the outcome is `confirmed` AND may
/// MOVE `resolved_line` — the one exception to D6, distinguished from the
/// token pass's `drifted` by `line_evidence` alone (E5/R18).
///
/// **This function's `Some` is not yet the shipped outcome.** DCB-W2.A.R fix
/// 2: `map_line` has no notion of the target file's actual length, so a hint
/// past the OLD state's effective EOF can still extrapolate to a `Mapped`
/// result past the NEW (working-tree) EOF too. `line_half` — the only
/// production caller — bound-checks `resolved_line`/`resolved_line_end`
/// against one memoised read of the working tree before shipping this
/// `Some`, demoting an out-of-range result back to arm 2 (and overwriting
/// `file_lines` with the real count when it keeps it). This function stays
/// pure on purpose: the bound-check needs I/O the caller already owns a
/// memo for, and mixing it in here would give this predicate two different
/// callers two different contracts.
pub fn remap_outcome(hint: u32, start: LineMap, end: Option<LineMap>) -> Option<LineOutcome> {
    let LineMap::Mapped(mapped) = start else {
        return None;
    };
    let mapped_end = match end {
        None => None,
        // A range whose end maps BEFORE its start is not a span at all.
        Some(LineMap::Mapped(e)) if e >= mapped => Some(e),
        Some(_) => return None,
    };
    Some(LineOutcome {
        state: LineState::Confirmed,
        evidence: LineEvidence::RevRemap,
        token: None,
        token_line: None,
        resolved_line: Some(mapped),
        resolved_line_end: mapped_end,
        line_hint_delta: Some(i64::from(mapped) - i64::from(hint)),
        file_lines: 0,
        reason: None,
    })
}

/// PURE over the file's text — no filesystem, no git, no `Store`. Every
/// line-state golden drives this directly.
///
/// `remap` is W2.A's arm 1 ([`remap_outcome`]); `None` when the doc declared
/// no usable `kb-code-rev`, in which case this is byte-for-byte W1.C's
/// two-pass token predicate. The argument is on the PURE entry point on
/// purpose: the arm order is a property of the predicate, not of one call
/// site.
pub fn line_state(text: &str, hint: u32, tokens: &[String], remap: Option<LineMap>) -> LineOutcome {
    if let Some(out) = remap.and_then(|m| remap_outcome(hint, m, None)) {
        return out;
    }
    line_state_in(&FileText::new(text.to_string()), hint, tokens)
}

/// W2.A §4 **arm 2** — the same token predicate over an already-memoised
/// [`FileText`], the hot path (a `path_list` ref with three spans costs ONE
/// file read AND one line-offset pass, not three).
///
/// Deliberately takes no `remap`: [`remap_outcome`] itself is file-FREE, so
/// mixing it in here would force a read this pure predicate has no memo for
/// (the caller's EOF bound-check, fix 2, uses its OWN memoised read instead).
/// Callers run [`remap_outcome`] first and only reach this on its `None`.
pub fn line_state_in(ft: &FileText, hint: u32, tokens: &[String]) -> LineOutcome {
    let file_lines = ft.line_count();
    if tokens.is_empty() {
        return LineOutcome::unverifiable("no_token", file_lines);
    }

    let hits_in = |lo: u32, hi: u32, tok: &str| -> Vec<u32> {
        let mut out = Vec::new();
        if lo > hi {
            return out;
        }
        for n in lo..=hi {
            if let Some(l) = ft.line(n) {
                if contains_word(l, tok) {
                    out.push(n);
                }
            }
        }
        out
    };

    let window = |half: u32| -> (u32, u32) {
        let lo = hint.saturating_sub(half).max(1);
        let hi = hint.saturating_add(half).min(file_lines);
        (lo, hi)
    };

    // --- Pass 1: confirm within ±CONFIRM_WINDOW_LINES -------------------
    let (lo3, hi3) = window(CONFIRM_WINDOW_LINES);
    let l3: Vec<Vec<u32>> = tokens.iter().map(|t| hits_in(lo3, hi3, t)).collect();
    let mut decisive: Vec<(usize, u32)> = l3
        .iter()
        .enumerate()
        .filter(|(_, h)| h.len() == 1)
        .map(|(i, h)| (i, h[0]))
        .collect();
    if !decisive.is_empty() {
        // Closest to the hint; tie → the earlier token in canonical order.
        decisive.sort_by_key(|(i, line)| (line.abs_diff(hint), *i));
        let (idx, line) = decisive[0];
        return LineOutcome {
            state: LineState::Confirmed,
            evidence: LineEvidence::ContextToken,
            token: Some(tokens[idx].clone()),
            token_line: Some(line),
            // D6 — the ±3 window is a tolerance on the EVIDENCE, not a
            // correction to the citation: a confirmed ref keeps the line the
            // document actually cited. Only W2.A's rev_remap arm may move it.
            resolved_line: Some(hint),
            resolved_line_end: None,
            line_hint_delta: Some(0),
            file_lines,
            reason: None,
        };
    }

    // --- Pass 2: drift within ±DRIFT_WINDOW_LINES ------------------------
    let (lo64, hi64) = window(DRIFT_WINDOW_LINES);
    let mut l64: Vec<Option<Vec<u32>>> = vec![None; tokens.len()];
    for (i, t) in tokens.iter().enumerate() {
        if l3[i].is_empty() {
            l64[i] = Some(hits_in(lo64, hi64, t));
        }
    }
    let mut drifted: Vec<(usize, u32)> = l64
        .iter()
        .enumerate()
        .filter_map(|(i, h)| match h {
            Some(h) if h.len() == 1 => Some((i, h[0])),
            _ => None,
        })
        .collect();
    if !drifted.is_empty() {
        drifted.sort_by_key(|(i, line)| (line.abs_diff(hint), *i));
        let (idx, line) = drifted[0];
        return LineOutcome {
            state: LineState::Drifted,
            evidence: LineEvidence::ContextToken,
            token: Some(tokens[idx].clone()),
            token_line: Some(line),
            resolved_line: Some(line),
            resolved_line_end: None,
            line_hint_delta: Some(i64::from(line) - i64::from(hint)),
            file_lines,
            reason: None,
        };
    }

    // --- Otherwise: unverifiable, with the pinned reason precedence ------
    let reason = if l3.iter().any(|h| h.len() >= 2) {
        "ambiguous_in_window"
    } else if l64.iter().any(|h| matches!(h, Some(h) if h.len() >= 2)) {
        "ambiguous_in_drift_window"
    } else {
        "token_not_found"
    };
    LineOutcome::unverifiable(reason, file_lines)
}

/// Parse `"30,51-65,113-119"` into ordered `(start, Option<end>)` pairs.
/// TOTAL: a malformed segment is skipped, and an all-malformed (or empty)
/// list falls back to the `line_start`/`line_end` pair the producer already
/// mirrored onto the ref.
pub fn parse_line_spans(
    spans: Option<&str>,
    start: Option<u32>,
    end: Option<u32>,
) -> Vec<(u32, Option<u32>)> {
    let mut out: Vec<(u32, Option<u32>)> = Vec::new();
    if let Some(raw) = spans {
        for seg in raw.split(',') {
            let seg = seg.trim();
            if seg.is_empty() {
                continue;
            }
            // Tolerate a legacy `L42` / `L42-L50` spelling.
            let seg = seg.trim_start_matches(['L', 'l']);
            match seg.split_once('-') {
                Some((a, b)) => {
                    let a = a.trim().parse::<u32>();
                    let b = b.trim().trim_start_matches(['L', 'l']).parse::<u32>();
                    if let (Ok(a), Ok(b)) = (a, b) {
                        if a >= 1 {
                            out.push((a, Some(b)));
                        }
                    }
                }
                None => {
                    if let Ok(a) = seg.parse::<u32>() {
                        if a >= 1 {
                            out.push((a, None));
                        }
                    }
                }
            }
        }
    }
    if out.is_empty() {
        if let Some(s) = start.filter(|s| *s >= 1) {
            out.push((s, end.filter(|e| *e != s)));
        }
    }
    out
}

// --- path resolution -------------------------------------------------------

/// Characters that must never reach a path lookup — defence in depth against
/// a glob or traversal that slipped past the producer's own hard-reject.
const UNUSABLE_PATH_CHARS: &[char] = &['*', '?', '[', ']', '"', '\'', '<', '>', '|'];

/// Strip a leading `./` / `/`, collapse `//`. `None` when the result is
/// unusable (empty, contains a `..` component, or contains a glob/quote char).
pub fn normalize_path_hint(hint: &str) -> Option<String> {
    let mut s = hint.trim();
    while let Some(rest) = s.strip_prefix("./") {
        s = rest;
    }
    let s = s.trim_start_matches('/');
    if s.is_empty() || s.contains(UNUSABLE_PATH_CHARS) {
        return None;
    }
    let parts: Vec<&str> = s.split('/').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() || parts.contains(&"..") {
        return None;
    }
    Some(parts.join("/"))
}

/// Resolve one ref's `path_hint` against the snapshot. Never fuzzy, never
/// frecency-weighted — `present`/`ambiguous`/`absent` are cardinalities.
pub fn resolve_path(snap: &RepoSnapshot, r: &CodeRefRow, repo: &str) -> PathResolution {
    // 1. Gem/vendor paths are the producer's classification; doc-lens never
    //    re-derives it and never resolves one against the tree.
    if r.kind == "external" {
        return PathResolution {
            state: Some(PathState::External),
            ..PathResolution::empty()
        };
    }

    // 2. Issues — BEFORE any normalisation (R11). Without this arm
    //    `acme/shopfront` normalises like a path, finds no candidates, and the
    //    ref renders `absent` with a `/search?q=shopfront` deep link INTO the
    //    code repo: a confident lie about a doc that only cited a ticket.
    if r.kind == "issue" {
        let parsed = r.path_hint.as_deref().and_then(|h| h.split_once('/'));
        return match (parsed, r.line_start) {
            (Some((owner, repo_name)), Some(number))
                if !owner.is_empty() && !repo_name.is_empty() && !repo_name.contains('/') =>
            {
                PathResolution {
                    issue: Some(IssueRef {
                        owner: owner.to_string(),
                        repo: repo_name.to_string(),
                        number,
                        href: format!("https://github.com/{owner}/{repo_name}/issues/{number}"),
                    }),
                    ..PathResolution::empty()
                }
            }
            _ => PathResolution {
                note: Some("unusable issue ref".to_string()),
                ..PathResolution::empty()
            },
        };
    }

    // 3. No path hint at all ⇒ `path_state: null` (D5), not a new enum value.
    let Some(hint) = r.path_hint.as_deref().filter(|h| !h.is_empty()) else {
        return PathResolution::empty();
    };

    // 4. Normalise, or refuse loudly.
    let Some(norm) = normalize_path_hint(hint) else {
        return PathResolution {
            state: Some(PathState::Absent),
            note: Some("unusable path".to_string()),
            ..PathResolution::empty()
        };
    };

    let search = |q: &str| {
        Some(SearchLink {
            q: q.to_string(),
            repo: repo.to_string(),
        })
    };

    // 5. Exact path wins outright.
    let mut out = if let Some(row) = snap.exact(&norm) {
        PathResolution {
            state: Some(PathState::Present),
            resolved: Some(row.path.clone()),
            candidate_count: 1,
            candidates: vec![row.path.clone()],
            ..PathResolution::empty()
        }
    } else {
        // 6/7. `/`-anchored suffix candidates, path-ascending (list_files is
        //      already ORDER BY path, so the bucket preserves that order).
        let cands: Vec<String> = snap
            .suffix_candidates(&norm)
            .into_iter()
            .map(|r| r.path.clone())
            .collect();
        match cands.len() {
            0 => PathResolution {
                state: Some(PathState::Absent),
                search: search(basename(&norm)),
                ..PathResolution::empty()
            },
            1 => PathResolution {
                state: Some(PathState::Present),
                resolved: Some(cands[0].clone()),
                candidate_count: 1,
                candidates: cands,
                ..PathResolution::empty()
            },
            n => PathResolution {
                state: Some(PathState::Ambiguous),
                candidate_count: n,
                // Over the inline tier the count is still exact; the list is
                // dropped in favour of the search link.
                candidates: if n <= AMBIGUITY_INLINE_MAX {
                    cands
                } else {
                    Vec::new()
                },
                search: search(basename(&norm)),
                ..PathResolution::empty()
            },
        }
    };

    // 9. Decision 2's doc-rot detector — verification is always on.
    if r.declared && out.state != Some(PathState::Present) {
        out.note = Some("declared but absent".to_string());
    }
    out
}

// --- symbol resolution -----------------------------------------------------

/// The name this ref asks about, if any (§9.5 step 1).
pub fn symbol_lookup_name(r: &CodeRefRow) -> Option<&str> {
    if let Some(m) = r.symbol_member.as_deref().filter(|m| !m.is_empty()) {
        return Some(m);
    }
    if r.kind == "symbol_const" {
        return r.symbol_container.as_deref().filter(|c| !c.is_empty());
    }
    None
}

pub fn resolve_symbol(
    by_name: &HashMap<String, Vec<(String, Symbol)>>,
    r: &CodeRefRow,
    resolved_path: Option<&str>,
) -> SymbolResolution {
    let Some(name) = symbol_lookup_name(r) else {
        return SymbolResolution::none();
    };
    let Some(rows) = by_name.get(name) else {
        return SymbolResolution::none();
    };

    // The ref named the file — a genuine, deterministic precision win.
    let scoped: Vec<&(String, Symbol)> = match resolved_path {
        Some(p) => rows.iter().filter(|(path, _)| path == p).collect(),
        None => rows.iter().collect(),
    };

    let to_hit = |(path, s): &&(String, Symbol)| SymbolHit {
        path: path.clone(),
        line_start: s.line_start,
        line_end: s.line_end,
        kind: s.kind.clone(),
        container: s.container.clone(),
    };

    let state = match scoped.len() {
        0 => return SymbolResolution::none(),
        1 => SymbolState::HitUnique,
        _ => match r.symbol_container.as_deref().filter(|c| !c.is_empty()) {
            Some(c) => {
                let survivors: Vec<&&(String, Symbol)> = scoped
                    .iter()
                    .filter(|(_, s)| match s.container.as_deref() {
                        Some(sc) => {
                            sc == c
                                || sc.ends_with(&format!("::{c}"))
                                || c.ends_with(&format!("::{sc}"))
                        }
                        None => false,
                    })
                    .collect();
                if survivors.len() == 1 {
                    let hit = to_hit(survivors[0]);
                    return SymbolResolution {
                        state: SymbolState::HitContainerMatched,
                        hit_count: scoped.len(),
                        hits: vec![hit],
                    };
                }
                SymbolState::HitAmbiguous
            }
            None => SymbolState::HitAmbiguous,
        },
    };

    SymbolResolution {
        state,
        hit_count: scoped.len(),
        hits: scoped.iter().take(MAX_SYMBOL_HITS).map(to_hit).collect(),
    }
}

// --- the request preamble (shared by both read routes) ---------------------

/// `pub(crate)` (not private) — W2.B's `wire::resolve_path_route` reuses this
/// SAME mapping for `KbClient::resolve_doc_by_path`'s errors, so a
/// kb-daemon-down/kb-daemon-disabled/kb-forbidden failure reads identically
/// on every doc-lens surface rather than growing a second ad-hoc mapping.
pub(crate) fn kb_client_error(e: KbClientError) -> ApiError {
    match e {
        KbClientError::Disabled => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "kb daemon federation is disabled ([kb_daemon] enabled = false)",
        )
        .with_reason("kb_daemon_disabled"),
        KbClientError::Unreachable(url, msg) => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            format!("kb daemon unreachable at {url}: {msg}"),
        )
        .with_reason("kb_unreachable"),
        KbClientError::ClientBuild(msg) => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            format!("kb daemon client unavailable: {msg}"),
        )
        .with_reason("kb_unreachable"),
        KbClientError::BadStatus(s)
            if s == StatusCode::UNAUTHORIZED || s == StatusCode::FORBIDDEN =>
        {
            ApiError::new(
                StatusCode::BAD_GATEWAY,
                format!("kb daemon refused this daemon's token ({s})"),
            )
            .with_reason("kb_forbidden")
        }
        KbClientError::BadStatus(s) => {
            ApiError::new(StatusCode::BAD_GATEWAY, format!("kb daemon returned {s}"))
                .with_reason("kb_upstream_error")
        }
        KbClientError::Parse(msg) => ApiError::new(
            StatusCode::BAD_GATEWAY,
            format!("kb daemon response was not coderef/1: {msg}"),
        )
        .with_reason("kb_upstream_error"),
        // kb-sibling/1 — the peer is UP and answering, so this is a 502
        // (bad upstream), never the 503 an unreachable/disabled kb gets;
        // its own reason string keeps the two distinguishable on every
        // doc-lens surface.
        KbClientError::SiblingMismatch(msg) => ApiError::new(
            StatusCode::BAD_GATEWAY,
            format!("kb daemon speaks a different sibling contract: {msg}"),
        )
        .with_reason("kb_sibling_mismatch"),
        // DCB-W2.B.R fix 1 (security) — the belt-and-braces guard inside
        // `KbClient::resolve_doc_by_path` itself; the route's own guard
        // (`wire::resolve_path_route`) always catches this first in
        // practice, but a future direct caller of the client still gets the
        // SAME 400/invalid_segment shape, never a 5xx.
        KbClientError::InvalidPath(msg) => {
            ApiError::bad_request_with_reason(msg, "invalid_segment")
        }
    }
}

/// Fetch the doc's `coderef/1` payload and keep the pin honest: a 404 drops
/// the pin LOUDLY, and a moves-301 re-keys it from the response BODY's
/// `doc_id` (D13/R13 — never from the redirect URL). Returns the payload plus
/// the id the caller ASKED for when it differs.
async fn fetch_doc(
    state: &SharedState,
    kb: &str,
    doc: &str,
) -> Result<(CodeRefsDoc, Option<String>), ApiError> {
    let fetched = state
        .kb_client
        .code_refs(kb, doc)
        .await
        .map_err(kb_client_error)?;
    let Some(d) = fetched else {
        // amendment 11 — a pin whose doc 404s is dropped, loudly; never
        // silently repaired, and never left pointing at a dead id.
        let kb_c = kb.to_string();
        let doc_c = doc.to_string();
        match state
            .store
            .run_blocking(move |store| store.delete_doc_lens_pin(&kb_c, &doc_c))
            .await
        {
            Ok(true) => tracing::warn!(kb, doc, "doc-lens: kb 404s this doc — dropped its pin"),
            Ok(false) => {}
            Err(e) => tracing::warn!(kb, doc, error = %e, "doc-lens: pin drop failed"),
        }
        return Err(ApiError::not_found(format!("kb {kb:?} has no doc {doc:?}"))
            .with_reason("doc_not_found"));
    };
    let moved_from = if d.doc_id != doc && !d.doc_id.is_empty() {
        let kb_c = kb.to_string();
        let doc_c = doc.to_string();
        let new_id = d.doc_id.clone();
        match state
            .store
            .run_blocking(move |store| store.rekey_doc_lens_pin(&kb_c, &doc_c, &new_id))
            .await
        {
            Ok(moved) => tracing::info!(
                kb,
                from = doc,
                to = %d.doc_id,
                pin_rekeyed = moved,
                "doc-lens: kb's moves chain re-keyed this doc"
            ),
            Err(e) => tracing::warn!(kb, doc, error = %e, "doc-lens: pin re-key failed"),
        }
        Some(doc.to_string())
    } else {
        None
    };
    Ok((d, moved_from))
}

/// A configured repo, resolved for THIS request. `source` is `"param"` or
/// `"pin"` — a checkout is NEVER auto-selected (Decision 1).
struct SelectedRepo {
    name: String,
    root: PathBuf,
    repo_id: i64,
    source: &'static str,
}

async fn select_repo(
    state: &SharedState,
    kb: &str,
    doc_id: &str,
    repo: Option<&str>,
) -> Result<SelectedRepo, ApiError> {
    if let Some(name) = repo.filter(|r| !r.is_empty()) {
        let (entry, repo_id) = find_repo(state, name)?;
        return Ok(SelectedRepo {
            name: entry.name.clone(),
            root: entry.path.clone(),
            repo_id,
            source: "param",
        });
    }
    // No `?repo=` — fall back to the remembered pin. This touches the store
    // (a pin lookup, and possibly a stale-pin delete) — one blocking-pool
    // round trip; `find_repo`/`root_matches` ride along on a cloned
    // `SharedState` since they only need the (cheap, in-memory) config, not
    // a second hop.
    let state_c = state.clone();
    let kb_c = kb.to_string();
    let doc_id_c = doc_id.to_string();
    state
        .store
        .run_blocking(move |_store| {
            if let Ok(Some(pin)) = state_c.store.get_doc_lens_pin(&kb_c, &doc_id_c) {
                if let Ok((entry, repo_id)) = find_repo(&state_c, &pin.repo) {
                    // The pin records the root it was CHOSEN against; a repo
                    // re-pointed under a stable name would otherwise silently
                    // select a tree the operator never picked (V0020's own
                    // rationale).
                    if super::pins::root_matches(&entry.path, &pin.repo_root) {
                        return Ok(SelectedRepo {
                            name: entry.name.clone(),
                            root: entry.path.clone(),
                            repo_id,
                            source: "pin",
                        });
                    }
                    // W2.A §6.2 — the read-time re-check, defence in depth
                    // BEHIND the boot prune (E7): config has no live reload,
                    // so this cannot fire in production today, which is
                    // exactly why it is here — it is what makes the
                    // behaviour testable without a restart. Deleting rather
                    // than ignoring: a stale pin that survives a read keeps
                    // pre-selecting a tree it was never chosen against on
                    // every subsequent one.
                    tracing::warn!(
                        kb = %kb_c,
                        doc = %doc_id_c,
                        repo = %pin.repo,
                        pinned_root = %pin.repo_root,
                        live_root = %entry.path.display(),
                        "doc-lens: dropping a pin whose repo root moved"
                    );
                    if let Err(e) = state_c.store.delete_doc_lens_pin(&kb_c, &doc_id_c) {
                        tracing::warn!(
                            kb = %kb_c, doc = %doc_id_c, error = %e,
                            "doc-lens: stale pin drop failed"
                        );
                    }
                    return Err(ApiError::bad_request(format!(
                        "the remembered checkout for this doc pointed at {}, which is no longer \
                         what {:?} resolves to — pick again \
                         (GET /api/doc-lens/repos?kb={kb_c}&doc={doc_id_c} scores every \
                         configured repo)",
                        pin.repo_root, entry.name
                    ))
                    .with_reason("repo_required"));
                }
            }
            Err(ApiError::bad_request(format!(
                "no checkout chosen for {kb_c}/{doc_id_c}: pass ?repo=<name> or pin one \
                 (GET /api/doc-lens/repos?kb={kb_c}&doc={doc_id_c} scores every configured repo)"
            ))
            .with_reason("repo_required"))
        })
        .await
}

// --- the blocking half -----------------------------------------------------

/// Per-repo git facts + index readiness, all of it inside the caller's
/// `spawn_blocking` (gix's `Repository` is `!Send`, and `git status` is a
/// subprocess).
struct RepoFacts {
    state: &'static str,
    reason: Option<String>,
    head_sha: Option<String>,
    head_branch: Option<String>,
    dirty: Option<bool>,
}

fn repo_facts(store: &Store, repo_id: i64, root: &Path) -> RepoFacts {
    let git = match GitRepo::open(root) {
        Ok(g) => g,
        Err(e) => {
            return RepoFacts {
                state: "error",
                reason: Some(format!("cannot open git repository: {e}")),
                head_sha: None,
                head_branch: None,
                dirty: None,
            }
        }
    };
    let indexed = store.file_count(repo_id).unwrap_or(0);
    if indexed == 0 {
        // The boot walk has not landed yet — distinct from `absent`, which
        // would read as doc-rot (amendment 5).
        return RepoFacts {
            state: "indexing",
            reason: Some("repo has no indexed files yet".to_string()),
            head_sha: None,
            head_branch: None,
            dirty: None,
        };
    }
    let head = git.head_info().ok();
    RepoFacts {
        state: "ready",
        reason: None,
        head_sha: head.as_ref().and_then(|h| h.sha.clone()),
        head_branch: head.as_ref().and_then(|h| h.branch.clone()),
        dirty: crate::repo_state::is_dirty(root).ok(),
    }
}

/// Read + memoise one repo-relative file, capped. The memo makes "a file is
/// read at most once per request" a structural property, not a convention.
fn file_text(
    memo: &mut HashMap<String, Result<Arc<FileText>, &'static str>>,
    root: &Path,
    rel: &str,
    size_hint: u64,
) -> Result<Arc<FileText>, &'static str> {
    if let Some(hit) = memo.get(rel) {
        return hit.clone();
    }
    let out = if size_hint > LENS_FILE_READ_CAP {
        Err("file_too_large")
    } else {
        match std::fs::read(root.join(rel)) {
            Err(_) => Err("unreadable"),
            Ok(bytes) if bytes.len() as u64 > LENS_FILE_READ_CAP => Err("file_too_large"),
            Ok(bytes) => match String::from_utf8(bytes) {
                Err(_) => Err("not_text"),
                Ok(text) => Ok(Arc::new(FileText::new(text))),
            },
        }
    };
    memo.insert(rel.to_string(), out.clone());
    out
}

struct ResolvedRefs {
    refs: Vec<RefOut>,
    partial: bool,
}

/// ONE symbols query for the whole request, keyed by every name any ref
/// wants to look up. Split out of [`resolve_refs_blocking`] (rather than
/// queried inline there) so a store failure can be surfaced to the SAME
/// `RepoFacts { state: "error", .. }` degrade path `RepoSnapshot::build`'s
/// own failure already uses — see the call site in `resolve_lens` (C2). A
/// bare `.unwrap_or_default()` here would silently turn a poisoned/wedged
/// store into a `no_symbol` verdict for every ref in the request, which
/// reads as "this repo genuinely has none of these symbols" rather than
/// "the lookup itself failed."
fn symbols_by_name(
    store: &Store,
    repo_id: i64,
    rows: &[CodeRefRow],
) -> Result<HashMap<String, Vec<(String, Symbol)>>, crate::store::StoreError> {
    let mut names: Vec<String> = rows
        .iter()
        .filter_map(|r| symbol_lookup_name(r).map(str::to_string))
        .collect();
    names.sort();
    names.dedup();
    Ok(store.symbols_named_many(repo_id, &names)?.into_iter().fold(
        HashMap::new(),
        |mut acc, (path, sym)| {
            acc.entry(sym.name.clone()).or_default().push((path, sym));
            acc
        },
    ))
}

/// The whole per-repo resolution pass. Runs inside ONE `spawn_blocking` —
/// which is also what makes `remap`'s `git` subprocesses legal here.
#[allow(clippy::too_many_lines)]
fn resolve_refs_blocking(
    snap: &RepoSnapshot,
    by_name: &HashMap<String, Vec<(String, Symbol)>>,
    rows: &[CodeRefRow],
    budget: Duration,
    remap: &mut RevRemap,
    at_declared: bool,
) -> ResolvedRefs {
    // The budget governs the RESOLUTION LOOP — the only unbounded part of the
    // request (N refs × file reads) — and is therefore started HERE rather
    // than at the top of `resolve_lens`. The preamble it excludes (one
    // `list_files`, one `git status`, one symbols query) is O(1) per request
    // and cannot be cancelled by a budget anyway; measuring from before the
    // `spawn_blocking` hop would instead charge the lens for however long the
    // blocking pool was queued, and degrade a perfectly resolvable doc to
    // all-`unverifiable` under unrelated load. Deliberately NOT
    // `tokio::time::timeout`, which cannot cancel a blocking task and would
    // leak it.
    let deadline = Instant::now() + budget;

    let mut memo: HashMap<String, Result<Arc<FileText>, &'static str>> = HashMap::new();
    let mut partial = false;
    let mut out = Vec::with_capacity(rows.len());

    // CT-F2 — built ONLY when both (a) the caller opted in via `at=declared`
    // and (b) the doc-level remap is `Applied` (a usable `kb-code-rev`: not
    // dirty, names THIS repo, sha resolves here). `None` in every other case,
    // so `when_written` short-circuits to `None` for the whole request
    // without touching git at all — the perf guard the work order asks for,
    // on top of the per-path memo [`DeclaredEraCache`] itself already keeps.
    let mut era_cache = if at_declared && remap.state == RemapState::Applied {
        remap
            .resolved_sha
            .clone()
            .map(|sha| DeclaredEraCache::new(snap.root.clone(), sha))
    } else {
        None
    };

    for r in rows {
        let path = resolve_path(snap, r, &snap.name);
        let symbol = resolve_symbol(by_name, r, path.resolved.as_deref());
        let tokens = confirm_tokens(r);

        // --- the line half, deadline-checked ---------------------------
        // The remap is inside the budget too: it spawns `git` per distinct
        // path, so an expired deadline must stop it exactly like it stops the
        // file reads.
        let (line, spans, remap_outcome) = if !partial && Instant::now() >= deadline {
            partial = true;
            (LineOutcome::unverifiable("deadline", 0), Vec::new(), None)
        } else if partial {
            (LineOutcome::unverifiable("deadline", 0), Vec::new(), None)
        } else {
            line_half(snap, &mut memo, r, &path, &tokens, remap)
        };

        // Same deadline discipline as the line half above: CT-F2's extra
        // git reads must stop the instant the budget trips, not add an
        // unbounded tail past it.
        let when_written = if partial {
            None
        } else {
            era_cache.as_mut().and_then(|c| when_written(c, r, &tokens))
        };

        out.push(build_ref_out(
            r,
            path,
            line,
            spans,
            symbol,
            remap_outcome,
            when_written,
            &snap.name,
        ));
    }

    ResolvedRefs { refs: out, partial }
}

/// The line-state half for ONE ref, including its per-span rollup.
///
/// Returns the per-ref `remap` wire string alongside the outcome: the FIRST
/// span's remap outcome (§9b.1 — a span-level field is not added; the ref-level
/// one plus each span's own `line_evidence` already says everything), and
/// `None` when the doc-level remap is not `Applied` at all.
fn line_half(
    snap: &RepoSnapshot,
    memo: &mut HashMap<String, Result<Arc<FileText>, &'static str>>,
    r: &CodeRefRow,
    path: &PathResolution,
    tokens: &[String],
    remap: &mut RevRemap,
) -> (LineOutcome, Vec<SpanOutcome>, Option<&'static str>) {
    // Order is pinned: an issue NUMBER must never be run through the line
    // predicate as if it were a line number.
    if r.kind == "issue" {
        return (LineOutcome::absent("issue"), Vec::new(), None);
    }
    if r.kind == "external" {
        return (LineOutcome::absent("external"), Vec::new(), None);
    }
    if r.line_start.is_none() {
        return (LineOutcome::absent("no_line_hint"), Vec::new(), None);
    }
    if path.state != Some(PathState::Present) {
        return (LineOutcome::absent("path_not_present"), Vec::new(), None);
    }
    let Some(rel) = path.resolved.as_deref() else {
        return (LineOutcome::absent("path_not_present"), Vec::new(), None);
    };

    let size_hint = snap.exact(rel).map(|f| f.size).unwrap_or(0);
    // The file read is LAZY: §4 arm 1 needs it only for the EOF bound (DCB-
    // W2.A.R fix 2) on a span it actually mapped — one memoised read per
    // PATH, never per span/ref — so a doc whose every citation remaps still
    // costs at most one read per distinct path, same as arm 2 always did.
    // `None` here means "not read yet", not "unreadable".
    let mut ft_slot: Option<Result<Arc<FileText>, &'static str>> = None;

    let pairs = parse_line_spans(r.line_spans.as_deref(), r.line_start, r.line_end);
    if pairs.is_empty() {
        // [B2b] `r.line_start` is `Some` (checked above) yet
        // `parse_line_spans` dropped every candidate span — a hostile or
        // pre-(a) producer can still put a zero/malformed line number on the
        // wire even after kb-core's own `first_span` refuses to mint one
        // (B2a covers the producer side; this is doc-lens's OWN tolerance
        // for a wire payload that skips that producer). An empty `spans: []`
        // with `line_reason: null` would read as "nothing to say," which is
        // indistinguishable from a ref that legitimately carries no line
        // hint at all — say so explicitly instead.
        let lines = read_lazily(&mut ft_slot, memo, &snap.root, rel, size_hint)
            .map(|ft| ft.line_count())
            .unwrap_or(0);
        return (
            LineOutcome::unverifiable("invalid_line_hint", lines),
            Vec::new(),
            None,
        );
    }

    let mut spans = Vec::with_capacity(pairs.len());
    let mut first_remap: Option<&'static str> = None;
    for (start, end) in pairs {
        // --- arm 1: the kb-code-rev remap, per span (§9b.1) -------------
        let span_remap = remap.map(rel, start);
        // The end endpoint is only ever consulted when the start mapped AND
        // the span has an end at all — the same precondition `remap_outcome`
        // itself checks, computed once here so the wire string below and the
        // mapped outcome read the SAME call rather than diverging.
        let end_remap = match (span_remap, end) {
            (Some(SpanRemap::Mapped(_)), Some(e)) => remap.map(rel, e),
            _ => None,
        };
        // [DCB-W2.A.R fix 3] The per-ref `remap` wire string is the FINAL
        // per-span decision, not the start endpoint's alone: a half-mapped
        // range is blocked by whichever endpoint actually failed, and that
        // endpoint's own outcome (e.g. `inside_change`) is the truthful
        // value for the rejection — reporting the start's bare `applied`
        // here (as originally shipped) is true of the start in isolation
        // but claims more than the span as a whole earned.
        let span_wire = end_remap.or(span_remap).map(SpanRemap::outcome);
        if first_remap.is_none() {
            first_remap = span_wire;
        }

        let mapped = match (span_remap.and_then(SpanRemap::line_map), end) {
            // E6 — a range maps BOTH endpoints or the whole span falls
            // through. `end_remap` is the SAME call `span_wire` above
            // already made — free, same path, same memo.
            (Some(LineMap::Mapped(s)), Some(_)) => end_remap
                .and_then(SpanRemap::line_map)
                .and_then(|em| remap_outcome(start, LineMap::Mapped(s), Some(em))),
            (Some(m), None) => remap_outcome(start, m, None),
            (Some(_), Some(_)) | (None, _) => None,
        };

        // [DCB-W2.A.R fix 2] EOF bound: `map_line` extrapolates past the
        // diff's own hunks once the hint sits beyond every hunk it saw — it
        // has no notion of "this file only has N lines" at all, so a
        // remapped line/range can land past the WORKING TREE's actual
        // length with nothing in arm 1 to notice (W1.C's honest
        // out-of-range golden, silently regressed whenever a kb-code-rev is
        // present). Bound-checked here against the one memoised read
        // `line_half` already owns — the ONLY read arm 1 ever forces, once
        // per PATH — and demoted to "arm 1 has nothing to say" on failure,
        // exactly like `InsideChange`, so arm 2 runs and reports honestly
        // with a real `file_lines`. A read failure demotes the same way;
        // arm 2's own `read_lazily` call below hits the identical memoised
        // error and reports it.
        let mapped = mapped.and_then(|o| {
            let ft = read_lazily(&mut ft_slot, memo, &snap.root, rel, size_hint).ok()?;
            let lines = ft.line_count();
            let in_range = o.resolved_line.is_some_and(|l| l <= lines)
                && o.resolved_line_end.is_none_or(|e| e <= lines);
            if in_range {
                Some(LineOutcome {
                    file_lines: lines,
                    ..o
                })
            } else {
                None
            }
        });

        // --- arm 2: W1.C's token pass, byte-for-byte unchanged ----------
        let o = match mapped {
            Some(o) => o,
            None => match read_lazily(&mut ft_slot, memo, &snap.root, rel, size_hint) {
                Ok(ft) => {
                    // Window uniqueness is PER SPAN —
                    // `carts_controller.rb:23,34` cites two lines holding the
                    // same text, and each is unique inside its own ±3
                    // window, so both confirm. A file-scoped rule would
                    // render both unverifiable.
                    line_state_in(&ft, start, tokens)
                }
                // [DCB-W2.A.R fix 7] An unreadable/oversized file no longer
                // SINKS THE WHOLE REF — a bare early `return` here used to
                // discard every span already resolved before this one,
                // including arm-1 git-verified evidence a `path_list`'s
                // earlier spans may have already earned. Every span of a
                // ref shares ONE resolved path (`rel`, computed once above
                // `line_half`'s per-span loop), and fix 2 just above now
                // forces this SAME memoised read on the first span that
                // needs one either way — arm 1's bound check or arm 2's own
                // fallback — so a genuine failure always surfaces on that
                // FIRST span, before anything has been pushed to `spans`,
                // and no span is actually lost today. Reporting the failure
                // per-span rather than sinking the ref keeps that true BY
                // CONSTRUCTION (every remaining span degrades honestly to
                // its own `unverifiable`/reason) rather than by coincidence
                // of call order, and costs nothing extra: the memo answers
                // every later span for this path instantly.
                Err(reason) => LineOutcome::unverifiable(reason, 0),
            },
        };
        spans.push(SpanOutcome {
            line_hint: start,
            line_hint_end: end,
            line_state: o.state,
            line_evidence: o.evidence,
            confirm_token: o.token.clone(),
            token_line: o.token_line,
            resolved_line: o.resolved_line,
            resolved_line_end: o.resolved_line_end,
            line_hint_delta: o.line_hint_delta,
            line_reason: o.reason,
        });
    }

    // Ref-level rollup: the WEAKEST span decides `line_state`; every other
    // field mirrors the FIRST span (which is also what `reader.line` points
    // at).
    let weakest = spans
        .iter()
        .map(|s| s.line_state)
        .min_by_key(|s| s.rank())
        .unwrap_or(LineState::Unverifiable);
    let first = spans.first();
    let rollup = LineOutcome {
        state: weakest,
        evidence: first.map(|s| s.line_evidence).unwrap_or(LineEvidence::None),
        token: first.and_then(|s| s.confirm_token.clone()),
        token_line: first.and_then(|s| s.token_line),
        resolved_line: first.and_then(|s| s.resolved_line),
        resolved_line_end: first.and_then(|s| s.resolved_line_end),
        line_hint_delta: first.and_then(|s| s.line_hint_delta),
        // `ft_slot` is populated whenever ANY span actually needed a read —
        // arm 1's EOF bound (fix 2) or arm 2's own fallback both go through
        // it — so this is non-zero for essentially every span with a real
        // path/line hint. `0` survives only for the paths that never reach
        // this loop's body at all: the deadline cut, and the early
        // `absent`/`invalid_line_hint` returns above it.
        file_lines: ft_slot
            .as_ref()
            .and_then(|r| r.as_ref().ok())
            .map(|ft| ft.line_count())
            .unwrap_or(0),
        reason: first.and_then(|s| s.line_reason),
    };
    (rollup, spans, first_remap)
}

/// Read-once-per-ref wrapper around [`file_text`]'s read-once-per-REQUEST
/// memo. Arm 1 now calls this too (fix 2's EOF bound), same as arm 2 always
/// did — the memo is what keeps that at most once per PATH regardless of how
/// many spans/arms touch it.
fn read_lazily(
    slot: &mut Option<Result<Arc<FileText>, &'static str>>,
    memo: &mut HashMap<String, Result<Arc<FileText>, &'static str>>,
    root: &Path,
    rel: &str,
    size_hint: u64,
) -> Result<Arc<FileText>, &'static str> {
    if slot.is_none() {
        *slot = Some(file_text(memo, root, rel, size_hint));
    }
    slot.clone().unwrap_or(Err("unreadable"))
}

// --- CT-F2: "when written" (era-resolved citations) -------------------------

/// CT-F2 — was a ref's citation true AT THE REV THE DOC DECLARED, as opposed
/// to the ever-present current-tree verdict above it? A doc written against
/// `<sha>` cites lines in THAT commit's own coordinate space, so this is
/// evaluated directly against `<sha>`'s content with NO remap involved
/// (`RevRemap`/`remap_outcome` map an OLD line to the CURRENT tree — the
/// opposite question from "was this ever true at all").
///
/// Reuses the exact same pure predicates the current-tree check runs
/// ([`path_exists_at_rev`], [`line_state_in`]) rather than forking a second
/// notion of "correct" — only the byte source differs (a `git show` blob vs
/// a working-tree read).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WhenWritten {
    path_state: PathState,
    line_state: LineState,
}

/// Per-request memo for [`when_written`]: at most one `cat-file -e` and one
/// `git show` per distinct PATH, no matter how many refs cite it. Keyed by
/// path alone (unlike [`RevRemap`]'s memo) because the doc's `code_rev`
/// names exactly ONE sha for the whole request — there is only ever one
/// "declared era" to check against.
struct DeclaredEraCache {
    root: PathBuf,
    sha: String,
    exists: HashMap<String, bool>,
    text: HashMap<String, Result<Arc<FileText>, &'static str>>,
}

impl DeclaredEraCache {
    fn new(root: PathBuf, sha: String) -> Self {
        Self {
            root,
            sha,
            exists: HashMap::new(),
            text: HashMap::new(),
        }
    }

    fn path_exists(&mut self, path: &str) -> bool {
        if let Some(v) = self.exists.get(path) {
            return *v;
        }
        // A transport failure (git missing, non-UTF8 stderr, …) reads as
        // "not there at that rev" — the SAME honest-absence posture
        // `RevRemap::map`'s callers already take on a git-call failure; there
        // is no second failure channel on this additive block to carry a
        // distinct "couldn't check" state through.
        let v = path_exists_at_rev(&self.root, &self.sha, path).unwrap_or(false);
        self.exists.insert(path.to_string(), v);
        v
    }

    fn text(&mut self, path: &str) -> Result<Arc<FileText>, &'static str> {
        if let Some(hit) = self.text.get(path) {
            return hit.clone();
        }
        let out = read_text_at_rev(&self.root, &self.sha, path, LENS_FILE_READ_CAP)
            .map(|t| Arc::new(FileText::new(t)));
        self.text.insert(path.to_string(), out.clone());
        out
    }
}

/// One ref's "when written" verdict, or `None` when there is nothing to
/// check against the declared rev at all: an issue/external ref (no path to
/// resolve — mirrors [`resolve_path`]'s own arms 1/2) or a ref carrying no
/// `path_hint` (D5). A hint that fails to normalise is `Absent`/`Absent`,
/// mirroring [`resolve_path`]'s own "unusable path" arm rather than treating
/// it as "nothing to check".
fn when_written(
    cache: &mut DeclaredEraCache,
    r: &CodeRefRow,
    tokens: &[String],
) -> Option<WhenWritten> {
    if r.kind == "issue" || r.kind == "external" {
        return None;
    }
    let hint = r.path_hint.as_deref().filter(|h| !h.is_empty())?;
    let Some(norm) = normalize_path_hint(hint) else {
        return Some(WhenWritten {
            path_state: PathState::Absent,
            line_state: LineState::Absent,
        });
    };
    if !cache.path_exists(&norm) {
        return Some(WhenWritten {
            path_state: PathState::Absent,
            line_state: LineState::Absent,
        });
    }
    // The FIRST parsed span's start line — same convention the ref-level
    // rollup already uses for `resolved_line`/`token_line` (§9b.1); a
    // per-span "when written" breakdown is not added (the ref-level pair is
    // what the work order asks for).
    let pairs = parse_line_spans(r.line_spans.as_deref(), r.line_start, r.line_end);
    let Some(&(start, _)) = pairs.first() else {
        return Some(WhenWritten {
            path_state: PathState::Present,
            line_state: LineState::Absent,
        });
    };
    let line_state = match cache.text(&norm) {
        Ok(ft) => line_state_in(&ft, start, tokens).state,
        Err(_) => LineState::Unverifiable,
    };
    Some(WhenWritten {
        path_state: PathState::Present,
        line_state,
    })
}

#[allow(clippy::too_many_arguments)]
fn build_ref_out(
    r: &CodeRefRow,
    path: PathResolution,
    line: LineOutcome,
    spans: Vec<SpanOutcome>,
    symbol: SymbolResolution,
    remap: Option<&'static str>,
    when_written: Option<WhenWritten>,
    repo: &str,
) -> RefOut {
    // Deep links are built from the RESOLUTION, never from the hint: the hint
    // is what the prose said, the resolution is what this daemon verified.
    let reader = match (&path.state, &path.resolved) {
        (Some(PathState::Present), Some(p)) => Some(ReaderTarget {
            repo: repo.to_string(),
            path: p.clone(),
            line: line.resolved_line,
        }),
        _ => None,
    };
    RefOut {
        ordinal: r.ordinal,
        group: r.group.clone(),
        kind: r.kind.clone(),
        raw: r.raw.clone(),
        declared: r.declared,
        path_hint: r.path_hint.clone(),
        line_hint: r.line_start,
        line_hint_end: r.line_end,
        symbol_container: r.symbol_container.clone(),
        symbol_member: r.symbol_member.clone(),
        context: r.context.clone(),
        path_state: path.state,
        resolved_path: path.resolved,
        candidate_count: path.candidate_count,
        candidates: path.candidates,
        issue: path.issue,
        line_state: line.state,
        line_evidence: line.evidence,
        confirm_token: line.token,
        token_line: line.token_line,
        resolved_line: line.resolved_line,
        resolved_line_end: line.resolved_line_end,
        line_hint_delta: line.line_hint_delta,
        file_lines: line.file_lines,
        line_reason: line.reason,
        remap,
        symbol_state: symbol.state,
        symbol_hit_count: symbol.hit_count,
        symbol_hits: symbol.hits,
        spans,
        reader,
        search: path.search,
        note: path.note,
        when_written: when_written.map(|w| WhenWrittenOut {
            path_state_at_rev: w.path_state,
            line_state_at_rev: w.line_state,
        }),
    }
}

fn counts_of(refs: &[RefOut], total: usize) -> LensCounts {
    let mut c = LensCounts {
        total,
        resolved: refs.len(),
        ..LensCounts::default()
    };
    for r in refs {
        match r.path_state {
            Some(PathState::Present) => c.present += 1,
            Some(PathState::Ambiguous) => c.ambiguous += 1,
            Some(PathState::Absent) => c.absent += 1,
            Some(PathState::External) => c.external += 1,
            None => {}
        }
        match r.line_state {
            LineState::Confirmed => c.confirmed += 1,
            LineState::Drifted => c.drifted += 1,
            LineState::Unverifiable => c.unverifiable += 1,
            LineState::Absent => c.line_absent += 1,
        }
        if r.declared && r.path_state != Some(PathState::Present) {
            c.declared_but_absent += 1;
        }
    }
    c
}

// --- the ONE entry point ---------------------------------------------------

/// The whole lens, gate included. `repo = None` means "use the pin, else 400
/// `repo_required`" — a checkout is NEVER auto-selected (Decision 1).
///
/// The `[doclens] kbs` gate lives HERE, not in the handler, so every caller
/// inherits it: W3.A's background sync must not pull a corpus's prose into
/// this daemon after an operator removed it from the allowlist.
///
/// [C1] `state.doclens.deadline_ms` bounds ONLY `resolve_refs_blocking`'s
/// inner loop, not this fn's own end-to-end wall-clock — see that budget's
/// doc comment (`DoclensSection::deadline_ms`) and `resolve_refs_blocking`'s
/// own comment on why it's timed from after the `spawn_blocking` hop, not
/// from here.
pub(crate) async fn resolve_lens(
    state: &SharedState,
    kb: &str,
    doc: &str,
    repo: Option<&str>,
    at_declared: bool,
) -> Result<CodeLensOut, ApiError> {
    gate(&state.doclens, kb, doc)?;
    let (d, moved_from) = fetch_doc(state, kb, doc).await?;
    let doc_id = if d.doc_id.is_empty() {
        doc.to_string()
    } else {
        d.doc_id.clone()
    };
    let picked = select_repo(state, kb, &doc_id, repo).await?;

    // Truncate by `ordinal` ascending; `counts.total` still reports the FULL
    // feed count — never a silent shrink.
    let mut rows = d.refs.clone();
    rows.sort_by_key(|r| r.ordinal);
    let feed_total = (d.ref_count as usize).max(rows.len());
    let cap = state.doclens.max_refs();
    let truncated_here = rows.len() > cap;
    rows.truncate(cap);

    let store = state.store.clone();
    let repo_id = picked.repo_id;
    let root = picked.root.clone();
    let name = picked.name.clone();
    let budget = Duration::from_millis(state.doclens.deadline_ms);
    let never_scanned = d.never_scanned;
    // W2.A — the doc's own `<meta name="kb-code-rev">`, moved into the
    // blocking half where `RevRemap::prepare`'s `git rev-parse` is legal.
    let code_rev = d.code_rev.clone();

    let (facts, resolved, rev_remap) = tokio::task::spawn_blocking(move || {
        let facts = repo_facts(&store, repo_id, &root);
        if facts.state != "ready" {
            return (facts, None, None);
        }
        let snap = match RepoSnapshot::build(&store, repo_id, &name, &root) {
            Ok(s) => s,
            Err(e) => {
                return (
                    RepoFacts {
                        state: "error",
                        reason: Some(format!("read the file index: {e}")),
                        ..facts
                    },
                    None,
                    None,
                )
            }
        };
        // [C2] Same degrade convention as the `RepoSnapshot::build` failure
        // just above: a store failure here becomes a request-level `"error"`
        // facts state, not a `no_symbol` verdict quietly stamped onto every
        // ref (a `.unwrap_or_default()` would have made that failure
        // indistinguishable from "this repo truly has none of these
        // symbols").
        let by_name = match symbols_by_name(&store, repo_id, &rows) {
            Ok(m) => m,
            Err(e) => {
                return (
                    RepoFacts {
                        state: "error",
                        reason: Some(format!("read the symbol index: {e}")),
                        ..facts
                    },
                    None,
                    None,
                )
            }
        };
        let mut remap = RevRemap::prepare(&root, &name, code_rev.as_ref());
        let resolved =
            resolve_refs_blocking(&snap, &by_name, &rows, budget, &mut remap, at_declared);
        // `null` on the wire when the doc declared no `kb-code-rev` at all
        // (§7): `doc_code_rev` is already null there, and a block whose only
        // content is "there was nothing to remap" is noise.
        let out = code_rev.is_some().then(|| RevRemapOut::from(&remap));
        (facts, Some(resolved), out)
    })
    .await
    .map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("doc-lens resolution task failed: {e}"),
        )
    })?;

    // A repo mid-boot-walk short-circuits rather than rendering an all-absent
    // lens that reads as doc-rot — unless there is nothing to resolve anyway.
    if facts.state == "indexing" && !never_scanned && d.ref_count > 0 {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            format!(
                "repo {:?} is still indexing ({})",
                picked.name,
                facts.reason.as_deref().unwrap_or("no indexed files yet")
            ),
        )
        .with_reason("repo_indexing"));
    }
    if facts.state == "error" {
        return Err(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(
                "repo {:?}: {}",
                picked.name,
                facts.reason.as_deref().unwrap_or("unavailable")
            ),
        )
        .with_reason("repo_unavailable"));
    }

    let (refs, partial) = match resolved {
        Some(r) => (r.refs, r.partial),
        None => (Vec::new(), false),
    };
    let counts = counts_of(&refs, if never_scanned { 0 } else { feed_total });

    // CT-F2 — "none" unless the caller opted in via `at=declared` AND the
    // doc's `kb-code-rev` was actually usable (`rev_remap.state ==
    // "applied"`): a doc with no usable rev honestly has nothing to compare
    // "when written" against, so every ref's `when_written` is `null` and
    // this says so at the envelope level rather than making a caller scan
    // `refs[]` to notice they're all absent.
    let era: &'static str =
        if at_declared && rev_remap.as_ref().is_some_and(|r| r.state == "applied") {
            "declared"
        } else {
            "none"
        };

    let groups = d
        .groups
        .iter()
        .map(|g| GroupOut {
            key: g.key.clone(),
            label: g.label.clone(),
            anchor: g.anchor.clone().unwrap_or_else(|| g.key.clone()),
            ordinal: g.ordinal,
            ref_count: refs
                .iter()
                .filter(|r| r.group.as_deref() == Some(g.key.as_str()))
                .count(),
        })
        .collect();

    Ok(CodeLensOut {
        schema: SCHEMA,
        kb: kb.to_string(),
        doc_id,
        moved_from,
        doc_path: d.doc_path.clone(),
        doc_href: d
            .doc_path
            .as_deref()
            .and_then(|p| doc_href(state.kb_daemon.public_base(), kb, p)),
        doc_hash: d.doc_hash.clone(),
        doc_title: d.title.clone(),
        doc_extracted_at: d.extracted_at,
        doc_code_rev: d.code_rev.as_ref().map(|c| CodeRevOut {
            repo_label: c.label.clone(),
            sha: c.sha.clone(),
            dirty: c.dirty,
        }),
        rev_remap,
        never_scanned,
        repo: RepoOut {
            name: picked.name,
            root: picked.root.to_string_lossy().to_string(),
            state: facts.state,
            head_sha: facts.head_sha,
            head_branch: facts.head_branch,
            dirty: facts.dirty,
            source: picked.source,
        },
        resolved_unix: chrono::Utc::now().timestamp(),
        truncated: d.truncated || truncated_here,
        partial,
        partial_reason: if partial { Some("deadline") } else { None },
        counts,
        ungrouped_count: d.ungrouped_count,
        groups,
        refs,
        era,
        note: LENS_NOTE,
    })
}

/// `GET /api/doc-lens/repos` — every configured repo scored against this doc.
///
/// NO `confirmed` column by ruling (D7): `confirmed` needs a file read per
/// distinct path × every configured repo, and `present/ambiguous/absent/
/// external` already decide the pick. Fan-out runs through
/// [`crate::fanout::buffered_join`] (V70-A1 — bounded, ORDER-PRESERVING:
/// `buffered`, not `buffer_unordered`) so `repos[]` is always config order —
/// the same submission-order determinism kb-server invariant #28 requires.
/// One broken repo yields `state: "error"` + `reason`, never a 500.
pub(crate) async fn resolve_scorecard(
    state: &SharedState,
    kb: &str,
    doc: &str,
) -> Result<ScorecardOut, ApiError> {
    gate(&state.doclens, kb, doc)?;
    let (d, _moved_from) = fetch_doc(state, kb, doc).await?;
    let doc_id = if d.doc_id.is_empty() {
        doc.to_string()
    } else {
        d.doc_id.clone()
    };

    let mut rows = d.refs.clone();
    rows.sort_by_key(|r| r.ordinal);
    let cap = state.doclens.max_refs();
    let truncated_here = rows.len() > cap;
    rows.truncate(cap);
    let rows = Arc::new(rows);

    // [DCB-W2.A.R fix 5] The scorecard is the ONE route the SPA's repo
    // picker actually reads `pinned_repo` off of, so it must carry the SAME
    // §6.2 read-time re-check `select_repo` already performs — a bare
    // `.map(|p| p.repo)` here would otherwise pre-select a tree the pin was
    // never chosen against, bypassing the defence entirely on the one path
    // that walks it. A stale pin (root moved) is dropped loudly, same as
    // `select_repo`'s own delete + warn; a pin whose repo is not configured
    // at all is left alone (nothing to re-check against) and simply answers
    // `null`, mirroring `select_repo`'s own asymmetry there.
    let state_c = state.clone();
    let kb_c = kb.to_string();
    let doc_id_c = doc_id.clone();
    let pinned_repo = state
        .store
        .run_blocking(move |_store| {
            match state_c
                .store
                .get_doc_lens_pin(&kb_c, &doc_id_c)
                .ok()
                .flatten()
            {
                Some(pin) => match find_repo(&state_c, &pin.repo) {
                    Ok((entry, _)) if super::pins::root_matches(&entry.path, &pin.repo_root) => {
                        Some(pin.repo)
                    }
                    Ok((entry, _)) => {
                        tracing::warn!(
                            kb = %kb_c,
                            doc = doc_id_c.as_str(),
                            repo = %pin.repo,
                            pinned_root = %pin.repo_root,
                            live_root = %entry.path.display(),
                            "doc-lens: dropping a pin whose repo root moved (scorecard read)"
                        );
                        if let Err(e) = state_c.store.delete_doc_lens_pin(&kb_c, &doc_id_c) {
                            tracing::warn!(
                                kb = %kb_c, doc = doc_id_c.as_str(), error = %e,
                                "doc-lens: stale pin drop failed"
                            );
                        }
                        None
                    }
                    Err(_) => None,
                },
                None => None,
            }
        })
        .await;

    let futs: Vec<futures::future::BoxFuture<'_, ScorecardRepoOut>> = state
        .repos
        .iter()
        .map(|entry| {
            let store = state.store.clone();
            let rows = rows.clone();
            let name = entry.name.clone();
            let root = entry.path.clone();
            let repo_id = state.repo_ids.get(&entry.name).copied();
            Box::pin(async move {
                tokio::task::spawn_blocking(move || {
                    score_one_repo(&store, &name, &root, repo_id, &rows)
                })
                .await
                .unwrap_or_else(|e| {
                    ScorecardRepoOut::error("", "", format!("scoring task failed: {e}"))
                })
            }) as futures::future::BoxFuture<'_, ScorecardRepoOut>
        })
        .collect();

    let repos: Vec<ScorecardRepoOut> =
        crate::fanout::buffered_join(futs, SCORECARD_FANOUT_CAP).await;

    Ok(ScorecardOut {
        schema: SCORECARD_SCHEMA,
        kb: kb.to_string(),
        doc_id,
        doc_hash: d.doc_hash.clone(),
        doc_title: d.title.clone(),
        never_scanned: d.never_scanned,
        resolved_unix: chrono::Utc::now().timestamp(),
        pinned_repo,
        counted_refs: rows.len(),
        truncated: d.truncated || truncated_here,
        repos,
        note: LENS_NOTE,
    })
}

fn score_one_repo(
    store: &Store,
    name: &str,
    root: &Path,
    repo_id: Option<i64>,
    rows: &[CodeRefRow],
) -> ScorecardRepoOut {
    let root_s = root.to_string_lossy().to_string();
    let Some(repo_id) = repo_id else {
        return ScorecardRepoOut::error(
            name,
            &root_s,
            "repo is configured but has no store id".to_string(),
        );
    };
    let facts = repo_facts(store, repo_id, root);
    if facts.state != "ready" {
        let mut out = ScorecardRepoOut::blank(name, &root_s, facts.state);
        out.reason = facts.reason;
        return out;
    }
    let snap = match RepoSnapshot::build(store, repo_id, name, root) {
        Ok(s) => s,
        Err(e) => {
            return ScorecardRepoOut::error(name, &root_s, format!("read the file index: {e}"))
        }
    };
    let (mut present, mut ambiguous, mut absent, mut external) = (0usize, 0usize, 0usize, 0usize);
    for r in rows {
        match resolve_path(&snap, r, name).state {
            Some(PathState::Present) => present += 1,
            Some(PathState::Ambiguous) => ambiguous += 1,
            Some(PathState::Absent) => absent += 1,
            Some(PathState::External) => external += 1,
            None => {}
        }
    }
    ScorecardRepoOut {
        name: name.to_string(),
        root: root_s,
        state: "ready",
        head_sha: facts.head_sha,
        head_branch: facts.head_branch,
        dirty: facts.dirty,
        present: Some(present),
        ambiguous: Some(ambiguous),
        absent: Some(absent),
        external: Some(external),
        partial: false,
        reason: None,
    }
}

// --- SL7e (v0.42, slate D29) — the PATH-addressed lens ---------------------

/// Resolve ONE caller-supplied repo path (± one line) against a checkout this
/// daemon mirrors, and answer `codelens-path/1`.
///
/// **Why it exists.** kb's slate board cites repo paths
/// (`path:crates/foo/src/bar.rs:141`) on posts that are not kb documents at
/// all, so neither doc-scoped read can answer for them: [`resolve_lens`]
/// needs a kb ARTIFACT ID and `resolve-path` maps a kb SOURCE path to that
/// id — a different question. This is the third question: *is this repo path
/// really there*.
///
/// **It forks no predicate.** The whole verdict is [`resolve_path`] +
/// [`line_state_in`], the exact functions [`resolve_lens`] runs, fed a
/// synthetic [`CodeRefRow`] — the input type those predicates already take —
/// rather than a second notion of "correct" written for this route. What
/// follows from that reuse, and is the load-bearing honesty property here:
/// with no `?context=` there is no `context` on the synthetic row, so
/// [`confirm_tokens`] returns an empty list — and a line with no confirm
/// token is `unverifiable` BY DEFINITION (this module's own rule), exactly
/// SL7e's shipped behaviour. **SL7f** wires the caller's optional
/// `?context=` onto that same synthetic row's `context` field, so
/// [`confirm_tokens`] runs its FALLBACK arm over it (there are never
/// producer-supplied `context_tokens` here — this route has no producer) —
/// the identical function a kb document's own prose feeds. `confirmed`/
/// `drifted` are reachable exactly when that scan yields a usable token;
/// still `unverifiable` when it does not (no context, or a context with no
/// token surviving the stoplist/length floor).
///
/// **Nothing is persisted and nothing is fetched.** No kb hop at all (the
/// `kb` param is ONLY the `[doclens] kbs` allowlist gate, same as every
/// sibling): invariant #2's one call direction is respected by making zero
/// calls. The only path ever joined to the repo root is `resolved` — a row
/// from this daemon's own `files` table, never the caller's bytes — so the
/// traversal check `resolve-path` needs (it interpolates its `path` into a
/// URL) has no counterpart here; a `..` hint is refused by
/// [`normalize_path_hint`] and reported as the honest `absent`.
pub(crate) async fn resolve_path_lens(
    state: &SharedState,
    kb: &str,
    path: &str,
    line: Option<u32>,
    repo: Option<&str>,
    context: Option<&str>,
) -> Result<PathLensOut, ApiError> {
    validate_kb_segment(kb)?;
    gate_kb_only(&state.doclens, kb)?;
    let hint = path.trim().to_string();
    if hint.is_empty() {
        return Err(ApiError::bad_request_with_reason(
            "path must not be empty",
            "invalid_segment",
        ));
    }
    let picked = pick_path_repo(state, repo)?;
    let repo_id = picked.repo_id;
    let root = picked.root.clone();
    let name = picked.name.clone();
    // SL7f: truncated, never rejected — an advisory field that only ever
    // narrows a verdict away from `unverifiable` (see `PATH_LENS_CONTEXT_MAX_CHARS`).
    let context: Option<String> = context.map(|c| {
        c.chars()
            .take(super::PATH_LENS_CONTEXT_MAX_CHARS)
            .collect::<String>()
    });

    state
        .store
        .run_blocking(move |store| {
            path_lens_blocking(
                store,
                repo_id,
                &name,
                &root,
                &hint,
                line,
                context.as_deref(),
            )
        })
        .await
}

/// Repo selection for the path lens. `?repo=` wins; with none, a daemon
/// serving exactly ONE checkout uses it and a daemon serving several refuses
/// — the `search::unified` text-lane rule ("needs exactly one repo — pass
/// ?repo="), which is also doc-lens's own "a checkout is NEVER
/// auto-selected". A single configured repo is not a pick between
/// alternatives; two are, and this route has no pin to remember one with
/// (the pin's PK is `(kb, doc)`, and there is no doc here).
fn pick_path_repo(state: &SharedState, repo: Option<&str>) -> Result<SelectedRepo, ApiError> {
    if let Some(name) = repo.filter(|r| !r.is_empty()) {
        let (entry, repo_id) = find_repo(state, name)?;
        return Ok(SelectedRepo {
            name: entry.name.clone(),
            root: entry.path.clone(),
            repo_id,
            source: "param",
        });
    }
    match state.repos.len() {
        0 => Err(
            ApiError::bad_request("this daemon has no repos configured".to_string())
                .with_reason("repo_required"),
        ),
        1 => {
            let (entry, repo_id) = find_repo(state, &state.repos[0].name)?;
            Ok(SelectedRepo {
                name: entry.name.clone(),
                root: entry.path.clone(),
                repo_id,
                source: "only",
            })
        }
        n => {
            let names: Vec<&str> = state.repos.iter().map(|r| r.name.as_str()).collect();
            Err(ApiError::bad_request(format!(
                "this daemon serves {n} checkouts ({}) — pass ?repo=<name>: a path lens \
                 against a checkout the caller never chose is a confident answer to the \
                 wrong question",
                names.join(", ")
            ))
            .with_reason("repo_required"))
        }
    }
}

/// The blocking half: one `file_count`, at most one `RepoSnapshot`, at most
/// one capped file read.
fn path_lens_blocking(
    store: &Store,
    repo_id: i64,
    name: &str,
    root: &Path,
    hint: &str,
    line: Option<u32>,
    context: Option<&str>,
) -> Result<PathLensOut, ApiError> {
    // Readiness, the CHEAP half of `repo_facts`: a repo whose boot walk has
    // not landed answers `absent` to everything, which reads as "your path is
    // gone" rather than "ask again in a minute" — `resolve_lens`' own
    // `repo_indexing` short-circuit, for the same reason. The git half of
    // `repo_facts` (head sha/branch + a `git status` subprocess) is skipped
    // deliberately: this response carries no git facts, so paying for a
    // subprocess per caption would be pure cost.
    match store.file_count(repo_id) {
        Ok(0) => {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                format!("repo {name:?} is still indexing (no indexed files yet)"),
            )
            .with_reason("repo_indexing"))
        }
        Err(e) => {
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("repo {name:?}: read the file index: {e}"),
            )
            .with_reason("repo_unavailable"))
        }
        Ok(_) => {}
    }

    // The synthetic ref. `kind` is from the producer's frozen set so the two
    // arms `resolve_path` keys on (`external`, `issue`) are provably not
    // taken; `declared: false` because a slate post is not a document
    // declaring a rev, so the doc-rot note has nothing to say here.
    // `context` (SL7f) carries the caller's `?context=` verbatim (already
    // length-capped by the caller) and `context_tokens` stays empty by
    // `CodeRefRow::default()` — there is no producer here, so
    // `confirm_tokens` always takes its FALLBACK (prose-scan) arm over this
    // field, exactly as it would for an older ref with no pre-extracted
    // tokens.
    let row = CodeRefRow {
        kind: if line.is_some() { "path_line" } else { "path" }.to_string(),
        path_hint: Some(hint.to_string()),
        line_start: line,
        context: context.map(str::to_string),
        ..CodeRefRow::default()
    };

    // Fast path, behaviour-identical BY CONSTRUCTION rather than by a second
    // implementation: `resolve_path` step 5 returns on `snap.exact(&norm)`
    // before `suffix_candidates` is ever consulted, so when the normalised
    // hint is an exact `files` row the answer is a function of THAT ROW
    // ALONE — a one-row snapshot and the whole-repo snapshot cannot differ.
    // Worth it because the board asks once per cited path per render, and
    // `RepoSnapshot::build` is `list_files` over the entire repo.
    let exact = normalize_path_hint(hint)
        .and_then(|norm| store.get_file(repo_id, &norm).ok().flatten())
        .map(|row| vec![row]);
    let snap = match exact {
        Some(rows) => RepoSnapshot::from_rows(repo_id, name, root, rows),
        None => RepoSnapshot::build(store, repo_id, name, root).map_err(|e| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("repo {name:?}: read the file index: {e}"),
            )
            .with_reason("repo_unavailable")
        })?,
    };
    let resolved = resolve_path(&snap, &row, name);

    // `line_state: null` is the FIFTH case, exactly as `path_state: null` is
    // for a ref carrying no path hint (D5): "no line verdict" is not one of
    // the four values and must not borrow one. `absent` in particular would
    // be read by a caption consumer as "the line is not there", which is a
    // claim neither an unasked question nor an unresolved path supports.
    let (line_state, line_reason, resolved_line, file_lines) =
        match (line, resolved.state, resolved.resolved.as_deref()) {
            (None, _, _) => (None, Some("no_line_hint"), None, None),
            (Some(hint_line), Some(PathState::Present), Some(rel)) => {
                let size_hint = snap.exact(rel).map(|f| f.size).unwrap_or(0);
                let mut memo: HashMap<String, Result<Arc<FileText>, &'static str>> = HashMap::new();
                // SL7f: `confirm_tokens` returns empty when `row.context` is
                // `None` (no `?context=`) — byte-identical to SL7e's `&[]` —
                // and otherwise runs the FALLBACK prose scan over it, the
                // SAME function/arm a kb document's own context feeds.
                let tokens = confirm_tokens(&row);
                match file_text(&mut memo, root, rel, size_hint) {
                    Ok(ft) => {
                        let o = line_state_in(&ft, hint_line, &tokens);
                        (Some(o.state), o.reason, o.resolved_line, Some(o.file_lines))
                    }
                    // `file_too_large`/`unreadable`/`not_text` — `line_half`'s
                    // own degrade, per-span there, whole-answer here.
                    Err(reason) => (Some(LineState::Unverifiable), Some(reason), None, None),
                }
            }
            (Some(_), _, _) => (None, Some("path_not_present"), None, None),
        };

    Ok(PathLensOut {
        schema: PATH_LENS_SCHEMA,
        repo: name.to_string(),
        path: hint.to_string(),
        line_hint: line,
        path_state: resolved.state,
        // `resolve_path`'s own note — today only "unusable path" (a hint
        // `normalize_path_hint` refuses: empty, or carrying a `..`).
        // Surfaced rather than swallowed: "this daemon would not read that
        // as a path at all" and "this checkout does not have that path" are
        // both `absent`, and a caller deserves to tell them apart.
        path_note: resolved.note,
        resolved_path: resolved.resolved,
        candidate_count: resolved.candidate_count,
        line_state,
        line_reason,
        resolved_line,
        file_lines,
        resolved_unix: chrono::Utc::now().timestamp(),
        note: PATH_LENS_NOTE,
    })
}

#[cfg(test)]
#[path = "resolve_tests.rs"]
mod tests;
