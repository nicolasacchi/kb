//! CT-F3 — **unlinked mentions**: the graph you wrote is half the graph you
//! meant. Obsidian's lesson, ported to kb's terms.
//!
//! A doc that says *"see the Deploy checklist before shipping"* is talking
//! about an artifact the corpus already holds — but unless somebody typed
//! `[[Deploy checklist]]`, the `edges` graph never learns it: no backlink,
//! no atlas link-line, no graph-report hub. This module is the pure,
//! deterministic, LLM-free engine that FINDS those missing threads and the
//! one that AUTHORS a real wikilink over one of them.
//!
//! Shape (deliberately the memory dupes/triage shape — `kb_core::triage`,
//! `GET /api/memory/dupes`):
//!
//! - **Derived, never persisted.** [`find_mentions`] is a read-time report
//!   over a corpus snapshot. No table, no column, no hook, no auto-linking:
//!   a suggestion is a claim re-computed on every read, exactly like a
//!   trust class in the doc↔code bridge (invariant #2).
//! - **A human applies it.** [`apply_wikilink`] is only ever reached from an
//!   explicit verb (`kb links apply`). Nothing here runs during indexing.
//! - **Honest refusals.** A mention whose source can't carry a wikilink is
//!   still REPORTED, flagged [`Applicability`]-false with the reason —
//!   never silently dropped, never "fixed" by rewriting HTML.
//!
//! **The memory ruling (invariant #29) stands untouched.** A memory body is
//! HTML wrapped in `<p>` by `crate::memory::text_to_body_html`, and comrak
//! finds ZERO wikilinks inside any `<p>`-wrapped content — so a memory can
//! be a link TARGET (and appears here as one) but never a link SOURCE. This
//! module encodes that as [`Applicability::MemoryHtml`] and refuses to
//! splice, rather than re-opening the twice-declined question.
//!
//! **One scanner.** Code spans, fenced blocks, existing `[[wikilinks]]` and
//! Markdown link labels are skipped by [`crate::links::prose_text`] — the
//! same comrak parse `parse_wikilinks` uses. There is no second grammar
//! here: the matcher below only decides *word boundaries and case*, and the
//! splice in [`apply_wikilink`] VERIFIES itself by re-running those same
//! two functions rather than trusting an offset.

use crate::links::{self, DocLite, Resolution, ResolveIndex};
use std::collections::{HashMap, HashSet};

/// Minimum length (in chars) of a title/basename that may be matched as a
/// mention. Short names collide with ordinary prose (`Notes`, `Deploy`,
/// `index.md`), and a false suggestion costs more than a missed one — the
/// queue is only useful if every row is worth reading. 12 follows the
/// CT-C5 `MIN_TITLE_LEN` precedent; it is a FLOOR, surfaced on the wire so
/// a caller can explain why a short-titled doc never appears.
pub const MIN_MENTION_LEN: usize = 12;

/// How a doc's body must be read before scanning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodySource {
    /// Raw Markdown source (the `.md` on disk, frontmatter already split
    /// off). Parsed with [`links::prose_text`], so code/links/wikilinks are
    /// skipped.
    Markdown,
    /// Already-extracted visible text — the lance `body` column of an HTML
    /// artifact. Scanned verbatim: the extraction happened at index time and
    /// this module never re-parses HTML (that would be the second html→text
    /// pipeline invariant #29 refuses).
    Text,
}

/// Which of a destination doc's names the mention matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchKind {
    /// The doc's exact title (case-insensitive).
    Title,
    /// The doc's filename — with or without its extension.
    Basename,
}

impl MatchKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MatchKind::Title => "title",
            MatchKind::Basename => "basename",
        }
    }
}

/// Can a wikilink actually be authored into this source?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applicability {
    /// A Markdown source: `[[…]]` in its body is parsed by the indexer's
    /// edge-record hook and becomes a real `kind="link"` edge.
    Markdown,
    /// An HTML **memory** body — invariant #29's twice-declined case.
    MemoryHtml,
    /// Any other HTML artifact.
    Html,
}

impl Applicability {
    /// True only for [`Applicability::Markdown`].
    pub fn is_applicable(self) -> bool {
        matches!(self, Applicability::Markdown)
    }

    /// The honest one-line reason a suggestion can't be applied, or `None`
    /// when it can. Both refusals name invariant #29 so the reader can look
    /// up the ruling rather than re-derive it.
    pub fn note(self) -> Option<&'static str> {
        match self {
            Applicability::Markdown => None,
            Applicability::MemoryHtml => {
                Some("cannot link from an HTML memory body — invariant #29")
            }
            Applicability::Html => Some(
                "cannot author a wikilink into an HTML artifact — the render path keeps \
                 [[…]] literal (invariant #29)",
            ),
        }
    }
}

/// Classify a source doc.
///
/// `is_markdown` MUST be the indexer's own gate
/// (`kb_core::indexer::is_markdown`) — the one the edge-record hook checks
/// before parsing `[[…]]` out of `raw_source` — and NOT the per-kb
/// render-time extension map. A kb that maps `.txt` to the Markdown
/// pipeline renders `[[…]]`, but the hook would never record the edge, so
/// "applicable" would be a lie.
pub fn applicability(is_markdown: bool, kb_category: Option<&str>) -> Applicability {
    if is_markdown {
        Applicability::Markdown
    } else if kb_category.is_some_and(|c| c.starts_with("memory-")) {
        Applicability::MemoryHtml
    } else {
        Applicability::Html
    }
}

/// One corpus doc, as the engine sees it. Every doc handed to
/// [`find_mentions`] contributes its NAMES (title/basename) as mention
/// targets; a doc with a non-empty `body` is additionally SCANNED as a
/// mention source. That split is load-bearing: the apply path passes the
/// whole corpus with exactly one body filled in, so it scans one file
/// while still resolving targets against the full corpus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MentionDoc {
    pub id: String,
    /// Source-relative path, forward-slash separated (`ops/deploy.md`).
    pub rel_path: String,
    pub title: String,
    /// Body to scan — empty means "target only, never scanned".
    pub body: String,
    pub source: BodySource,
    pub applicability: Applicability,
}

/// One unlinked mention: `src`'s body names `dst`, and no `kind="link"`
/// edge `src → dst` exists yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mention {
    pub src_id: String,
    pub dst_id: String,
    /// The text exactly as it appears in the source (original case) — what
    /// [`apply_wikilink`] splices over, and what a human reads to judge the
    /// suggestion.
    pub matched: String,
    pub kind: MatchKind,
    /// The wikilink target to author — see [`unique_target_for`]: the
    /// matched text itself when that already resolves back to `dst` (the
    /// sentence then needs no alias at all), else the dst's title,
    /// filename or path. A mention whose dst has no uniquely-resolving form
    /// is dropped: applying must produce a real edge, never a dangling
    /// `[[…]]`.
    pub target: String,
    pub applicability: Applicability,
}

/// Find every unlinked mention in `docs`, newest guard rails first:
///
/// - **self skipped** (a doc naming itself is not a link),
/// - **already-edged pairs skipped** — `existing_edges` are the corpus's
///   `(src, dst)` `kind="link"` pairs (`Storage::link_pairs`). The check is
///   DIRECTIONAL: `dst → src` existing does not suppress `src → dst`,
///   because the thread this queue is about (src's prose naming dst) is
///   genuinely unwritten in that direction,
/// - **code/links skipped** — via [`links::prose_text`] for Markdown bodies,
/// - **short names skipped** — [`MIN_MENTION_LEN`],
/// - **ambiguous names skipped** — a title/basename shared by two docs is
///   dropped wholesale (the same call [`links::resolve`]'s ladder makes:
///   an ambiguous name is not an honest link),
/// - **one row per (src, dst)** — the first (earliest) mention wins.
///
/// Order is deterministic and human-shaped: applicable rows first, then by
/// the source's path, then by where the mention sits in the body, then by
/// dst id. `limit` truncates AFTER that ordering.
pub fn find_mentions(
    docs: &[MentionDoc],
    existing_edges: &[(String, String)],
    limit: usize,
) -> Vec<Mention> {
    if limit == 0 || docs.is_empty() {
        return Vec::new();
    }
    let edges: HashSet<(&str, &str)> = existing_edges
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let by_id: HashMap<&str, &MentionDoc> = docs.iter().map(|d| (d.id.as_str(), d)).collect();
    let needles = build_needles(docs);
    if needles.is_empty() {
        return Vec::new();
    }

    // Target resolution runs against the SAME candidate set the corpus's own
    // wikilinks resolve against, so an applied link resolves the way the
    // edge-record hook will resolve it at the next index pass.
    let candidates: Vec<DocLite> = docs
        .iter()
        .map(|d| DocLite {
            id: d.id.clone(),
            rel_path: d.rel_path.clone(),
            title: d.title.clone(),
        })
        .collect();
    let resolver = ResolveIndex::new(&candidates);

    // (applicable_rank, src rel_path, offset, dst id, mention)
    let mut rows: Vec<(u8, &str, usize, &str, Mention)> = Vec::new();
    for src in docs.iter().filter(|d| !d.body.is_empty()) {
        let scan = match src.source {
            BodySource::Markdown => links::prose_text(&src.body),
            BodySource::Text => src.body.clone(),
        };
        // The `accept` closure filters self + already-edged destinations
        // before the (relatively expensive) verification; the one-row-per-pair
        // rule is applied while consuming, since a `for` head's temporaries
        // outlive the loop body and would freeze `paired`.
        let hits = scan_hits(&scan, &needles, |dst| {
            dst != src.id && !edges.contains(&(src.id.as_str(), dst))
        });
        let mut paired: HashSet<&str> = HashSet::new();
        for (offset, hit) in hits {
            let dst_id = hit.dst;
            if paired.contains(dst_id) {
                continue;
            }
            let target = by_id
                .get(dst_id)
                .and_then(|d| unique_target_for(d, &hit.matched, &resolver));
            let Some(target) = target else {
                // No form of this dst's name resolves back to it — applying
                // could only write a dangling link, so there is nothing
                // honest to suggest.
                continue;
            };
            paired.insert(dst_id);
            rows.push((
                u8::from(!src.applicability.is_applicable()),
                src.rel_path.as_str(),
                offset,
                dst_id,
                Mention {
                    src_id: src.id.clone(),
                    dst_id: dst_id.to_string(),
                    matched: hit.matched,
                    kind: hit.kind,
                    target,
                    applicability: src.applicability,
                },
            ));
        }
    }
    rows.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.cmp(b.1))
            .then_with(|| a.2.cmp(&b.2))
            .then_with(|| a.3.cmp(b.3))
    });
    rows.truncate(limit);
    rows.into_iter().map(|(_, _, _, _, m)| m).collect()
}

/// Author `[[target]]` (or `[[target|matched]]`, when the prose case differs
/// from the target) over the FIRST spliceable occurrence of `matched` in
/// `body_md`, returning the rewritten body. `None` = an honest refusal, and
/// the caller must NOT write anything:
///
/// - the texts carry wikilink syntax (`[[`, `]]`, `|`) or a newline,
/// - `matched` no longer appears in the body's PROSE (the source changed
///   under the suggestion, or the mention only ever existed in code),
/// - or no raw occurrence survives verification.
///
/// **Verification, not offset arithmetic.** [`links::prose_text`] flattens
/// the body, so its offsets do not map back to the source; instead every
/// candidate raw occurrence is spliced speculatively and then checked with
/// the two EXISTING scanners: [`links::parse_wikilinks`] must now see a
/// wikilink pointing at `target` (a splice inside a fence produces none),
/// and the prose occurrence count must have dropped by exactly one (a
/// splice inside an existing link label leaves prose unchanged, because
/// `prose_text` skips both). Only a candidate satisfying both wins. That is
/// how a body containing the same phrase in prose AND in a code fence gets
/// linked in the prose — with no second scanner to keep in step.
pub fn apply_wikilink(body_md: &str, matched: &str, target: &str) -> Option<String> {
    if matched.is_empty() || target.is_empty() {
        return None;
    }
    if [matched, target]
        .iter()
        .any(|s| s.contains("[[") || s.contains("]]") || s.contains('|') || s.contains('\n'))
    {
        return None;
    }
    let link = if matched == target {
        format!("[[{target}]]")
    } else {
        format!("[[{target}|{matched}]]")
    };
    let lower = matched.to_lowercase();
    let chars = matched.chars().count();
    let baseline = find_all(&links::prose_text(body_md), &lower, chars).len();
    if baseline == 0 {
        return None;
    }
    for (start, end) in find_all(body_md, &lower, chars) {
        let mut candidate = String::with_capacity(body_md.len() + link.len());
        candidate.push_str(&body_md[..start]);
        candidate.push_str(&link);
        candidate.push_str(&body_md[end..]);
        let became_link = links::parse_wikilinks(&candidate)
            .iter()
            .any(|w| w.target == target);
        let prose_left = find_all(&links::prose_text(&candidate), &lower, chars).len();
        if became_link && prose_left + 1 == baseline {
            return Some(candidate);
        }
    }
    None
}

// --- internals ---------------------------------------------------------------

/// A registered mention target: one of a doc's names, lowercased.
struct Needle<'a> {
    lower: String,
    chars: usize,
    dst: &'a str,
    kind: MatchKind,
}

/// One located mention inside a scanned body.
struct Hit<'a> {
    matched: String,
    dst: &'a str,
    kind: MatchKind,
}

/// A word char for boundary purposes: alphanumeric or `_`, so `deploy` never
/// matches inside `redeployment` or `deploy_v2`, while `my_note` stays one
/// word.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Trim a name down to its word-char core (`"«Deploy checklist»!"` →
/// `"Deploy checklist"`), so every needle both starts and ends on a word
/// boundary and can be anchored at its first word.
fn trim_to_word(s: &str) -> &str {
    let start = s.find(is_word_char).unwrap_or(s.len());
    let end = s.rfind(is_word_char).map_or(start, |i| {
        i + s[i..].chars().next().map_or(0, char::len_utf8)
    });
    &s[start..end]
}

/// The first word of a needle, lowercased — the scan's anchor.
fn first_word_lower(needle: &str) -> Option<String> {
    let rest = needle.trim_start_matches(|c: char| !is_word_char(c));
    let end = rest.find(|c: char| !is_word_char(c)).unwrap_or(rest.len());
    (end > 0).then(|| rest[..end].to_lowercase())
}

/// Build the first-word-anchored needle index over every doc's names.
/// Ambiguity is resolved the way [`links::resolve`] resolves it: a name
/// claimed by two different docs is dropped, not arbitrated.
fn build_needles(docs: &[MentionDoc]) -> HashMap<String, Vec<Needle<'_>>> {
    // lowercased name → (dst ids that claim it, kind of the first claim)
    let mut claims: HashMap<String, (Vec<&str>, MatchKind)> = HashMap::new();
    for d in docs {
        let mut seen: HashSet<String> = HashSet::new();
        let names = [
            (d.title.as_str(), MatchKind::Title),
            (links::basename(&d.rel_path), MatchKind::Basename),
            (links::stem(&d.rel_path), MatchKind::Basename),
        ];
        for (raw, kind) in names {
            let name = trim_to_word(raw);
            if name.chars().count() < MIN_MENTION_LEN {
                continue;
            }
            let lower = name.to_lowercase();
            if !seen.insert(lower.clone()) {
                continue;
            }
            let entry = claims.entry(lower).or_insert_with(|| (Vec::new(), kind));
            entry.0.push(d.id.as_str());
        }
    }
    let mut index: HashMap<String, Vec<Needle<'_>>> = HashMap::new();
    for (lower, (dsts, kind)) in claims {
        if dsts.len() != 1 {
            continue; // ambiguous name — never an honest suggestion
        }
        let Some(anchor) = first_word_lower(&lower) else {
            continue;
        };
        let chars = lower.chars().count();
        index.entry(anchor).or_default().push(Needle {
            lower,
            chars,
            dst: dsts[0],
            kind,
        });
    }
    // Longest first so `deploy-checklist.md` wins over `deploy-checklist` at
    // the same position; the lowercase tie-break keeps the order total.
    for bucket in index.values_mut() {
        bucket.sort_by(|a, b| {
            b.chars
                .cmp(&a.chars)
                .then_with(|| a.lower.cmp(&b.lower))
                .then_with(|| a.dst.cmp(b.dst))
        });
    }
    index
}

/// Walk `scan`'s word starts, probing the needle index at each. `accept`
/// filters candidate destinations (self / already-edged) BEFORE the
/// verification, longest-match-wins at a given position, and matches are
/// NON-OVERLAPPING left-to-right (the same rule [`find_all`] uses): a
/// phrase already consumed by `dataviz-guide.md` must not also be reported
/// as a mention of `guide.md`. Returns `(offset, hit)` in document order.
fn scan_hits<'a>(
    scan: &str,
    needles: &HashMap<String, Vec<Needle<'a>>>,
    mut accept: impl FnMut(&str) -> bool,
) -> Vec<(usize, Hit<'a>)> {
    let mut out: Vec<(usize, Hit<'a>)> = Vec::new();
    let mut buf = String::new();
    let mut prev_word = false;
    let mut skip_to = 0usize;
    for (i, ch) in scan.char_indices() {
        let word = is_word_char(ch);
        if !word || prev_word {
            prev_word = word;
            continue;
        }
        prev_word = true;
        if i < skip_to {
            continue;
        }
        // Lowercase the word into a reused buffer — a fresh String per word
        // would allocate once per word of the whole corpus.
        buf.clear();
        for c in scan[i..].chars().take_while(|c| is_word_char(*c)) {
            buf.extend(c.to_lowercase());
        }
        let Some(bucket) = needles.get(buf.as_str()) else {
            continue;
        };
        for needle in bucket {
            if !accept(needle.dst) {
                continue;
            }
            if let Some(end) = match_at(scan, i, &needle.lower, needle.chars) {
                out.push((
                    i,
                    Hit {
                        matched: scan[i..end].to_string(),
                        dst: needle.dst,
                        kind: needle.kind,
                    },
                ));
                skip_to = end;
                break; // longest verified match at this position wins
            }
        }
    }
    out
}

/// Does `needle_lower` (already lowercased, `needle_chars` long) occur at
/// byte offset `at` in `hay`, case-insensitively and ending on a word
/// boundary? Returns the end offset. The LEADING boundary is the caller's
/// (matches only start at word starts).
fn match_at(hay: &str, at: usize, needle_lower: &str, needle_chars: usize) -> Option<usize> {
    let mut end = at;
    let mut taken = 0usize;
    for (i, ch) in hay[at..].char_indices() {
        if taken == needle_chars {
            end = at + i;
            break;
        }
        taken += 1;
        end = at + i + ch.len_utf8();
    }
    if taken < needle_chars {
        return None;
    }
    if hay[at..end].to_lowercase() != needle_lower {
        return None;
    }
    if hay[end..].chars().next().is_some_and(is_word_char) {
        return None;
    }
    Some(end)
}

/// Every non-overlapping, word-bounded, case-insensitive occurrence of a
/// needle in `hay`, as `(start, end)` byte offsets — the same matcher
/// [`scan_hits`] uses, without the first-word index (one needle, so the
/// index would cost more than it saves).
fn find_all(hay: &str, needle_lower: &str, needle_chars: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    if needle_chars == 0 {
        return out;
    }
    let mut prev_word = false;
    let mut skip_to = 0usize;
    for (i, ch) in hay.char_indices() {
        let word = is_word_char(ch);
        let start_of_word = word && !prev_word;
        prev_word = word;
        if !start_of_word || i < skip_to {
            continue;
        }
        if let Some(end) = match_at(hay, i, needle_lower, needle_chars) {
            out.push((i, end));
            skip_to = end;
        }
    }
    out
}

/// The first form that resolves UNIQUELY back to `dst` through the corpus's
/// own ladder ([`links::ResolveIndex`]):
///
/// 1. **the matched text itself** — when the words already in the prose
///    resolve to the destination, the splice needs no alias and the
///    sentence survives untouched (`[[deployment checklist]]`),
/// 2. the dst's **title**, 3. its **basename stem**, 4. its **basename**,
/// 5. its **source-relative path** (tier 2, unique by construction).
///
/// Every candidate is VERIFIED against the resolver rather than assumed:
/// a title can be ambiguous, and a path-shaped title can even resolve to
/// someone else's file (tier 2 beats tier 3). `None` — no form resolves
/// back — is the engine's cue to drop the mention: a suggestion that would
/// author a dangling or misdirected link is worse than no suggestion.
fn unique_target_for(
    dst: &MentionDoc,
    matched: &str,
    resolver: &ResolveIndex<'_>,
) -> Option<String> {
    let candidates = [
        matched.trim(),
        dst.title.trim(),
        links::stem(&dst.rel_path),
        links::basename(&dst.rel_path),
        dst.rel_path.as_str(),
    ];
    for cand in candidates {
        if cand.is_empty() {
            continue;
        }
        if matches!(resolver.resolve(cand), Resolution::One(ref id) if *id == dst.id) {
            return Some(cand.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn md(id: &str, rel: &str, title: &str, body: &str) -> MentionDoc {
        MentionDoc {
            id: id.into(),
            rel_path: rel.into(),
            title: title.into(),
            body: body.into(),
            source: BodySource::Markdown,
            applicability: Applicability::Markdown,
        }
    }

    fn html(id: &str, rel: &str, title: &str, body: &str, memory: bool) -> MentionDoc {
        MentionDoc {
            id: id.into(),
            rel_path: rel.into(),
            title: title.into(),
            body: body.into(),
            source: BodySource::Text,
            applicability: if memory {
                Applicability::MemoryHtml
            } else {
                Applicability::Html
            },
        }
    }

    const NO_EDGES: &[(String, String)] = &[];

    #[test]
    fn finds_an_exact_title_mention() {
        let docs = [
            md(
                "src1",
                "ops/runbook.md",
                "Ops runbook",
                "Read the Deployment checklist before shipping.",
            ),
            md("dst1", "ops/deploy.md", "Deployment checklist", ""),
        ];
        let got = find_mentions(&docs, NO_EDGES, 10);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].src_id, "src1");
        assert_eq!(got[0].dst_id, "dst1");
        assert_eq!(got[0].matched, "Deployment checklist");
        assert_eq!(got[0].kind, MatchKind::Title);
        assert_eq!(got[0].target, "Deployment checklist");
        assert!(got[0].applicability.is_applicable());
    }

    #[test]
    fn title_match_is_case_insensitive_and_word_bounded() {
        let docs = [
            md(
                "src1",
                "a/notes.md",
                "Notes",
                "we ran the deployment checklist twice",
            ),
            md("dst1", "ops/deploy.md", "Deployment checklist", ""),
        ];
        let got = find_mentions(&docs, NO_EDGES, 10);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].matched, "deployment checklist", "original case kept");

        // Glued into a longer word ⇒ not a mention.
        let docs = [
            md(
                "src1",
                "a/notes.md",
                "Notes",
                "predeployment checklistify happened",
            ),
            md("dst1", "ops/deploy.md", "Deployment checklist", ""),
        ];
        assert!(find_mentions(&docs, NO_EDGES, 10).is_empty());
    }

    #[test]
    fn finds_a_basename_mention_and_prefers_the_longer_name() {
        let docs = [
            md(
                "src1",
                "a/notes.md",
                "Notes",
                "documented in deployment-checklist.md today",
            ),
            md("dst1", "ops/deployment-checklist.md", "Ship it", ""),
        ];
        let got = find_mentions(&docs, NO_EDGES, 10);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].kind, MatchKind::Basename);
        assert_eq!(
            got[0].matched, "deployment-checklist.md",
            "basename-with-extension beats the bare stem at the same position"
        );
        assert_eq!(
            got[0].target, "deployment-checklist.md",
            "the matched text already resolves — no alias needed"
        );
    }

    #[test]
    fn skips_mentions_inside_code_spans_and_fences() {
        // The guard rail that must reuse links.rs's scanner rather than
        // grow a second one.
        let docs = [
            md(
                "src1",
                "a/notes.md",
                "Notes",
                "run `Deployment checklist` now\n\n```\nDeployment checklist\n```\n",
            ),
            md("dst1", "ops/deploy.md", "Deployment checklist", ""),
        ];
        assert!(find_mentions(&docs, NO_EDGES, 10).is_empty());
    }

    #[test]
    fn skips_self_mentions() {
        let docs = [md(
            "src1",
            "ops/deploy.md",
            "Deployment checklist",
            "This IS the Deployment checklist, obviously.",
        )];
        assert!(find_mentions(&docs, NO_EDGES, 10).is_empty());
    }

    #[test]
    fn skips_names_under_the_floor() {
        // "Deploy" (6) and "deploy.md" (9) are both under MIN_MENTION_LEN.
        assert!(MIN_MENTION_LEN > "deploy.md".len());
        let docs = [
            md(
                "src1",
                "a/notes.md",
                "Notes",
                "the Deploy step failed again",
            ),
            md("dst1", "ops/deploy.md", "Deploy", ""),
        ];
        assert!(find_mentions(&docs, NO_EDGES, 10).is_empty());
    }

    #[test]
    fn skips_an_already_edged_pair_but_only_in_that_direction() {
        let docs = [
            md(
                "src1",
                "a/notes.md",
                "Notes",
                "see the Deployment checklist",
            ),
            md("dst1", "ops/deploy.md", "Deployment checklist", ""),
        ];
        let edged = [("src1".to_string(), "dst1".to_string())];
        assert!(
            find_mentions(&docs, &edged, 10).is_empty(),
            "src → dst edge suppresses the suggestion"
        );
        let reverse = [("dst1".to_string(), "src1".to_string())];
        assert_eq!(
            find_mentions(&docs, &reverse, 10).len(),
            1,
            "a dst → src backlink leaves src's own missing thread unwritten"
        );
    }

    #[test]
    fn skips_an_existing_wikilink_and_link_label() {
        let docs = [
            md(
                "src1",
                "a/notes.md",
                "Notes",
                "see [[Deployment checklist]] and [Deployment checklist](https://x/)",
            ),
            md("dst1", "ops/deploy.md", "Deployment checklist", ""),
        ];
        assert!(find_mentions(&docs, NO_EDGES, 10).is_empty());
    }

    #[test]
    fn ambiguous_names_are_dropped_wholesale() {
        let docs = [
            md(
                "src1",
                "a/notes.md",
                "Notes",
                "see the Deployment checklist",
            ),
            md("dst1", "ops/deploy.md", "Deployment checklist", ""),
            md("dst2", "infra/deploy.md", "deployment CHECKLIST", ""),
        ];
        assert!(
            find_mentions(&docs, NO_EDGES, 10).is_empty(),
            "two docs claim the name — the resolver would call it Ambiguous"
        );
    }

    #[test]
    fn one_row_per_pair_first_mention_wins() {
        let docs = [
            md(
                "src1",
                "a/notes.md",
                "Notes",
                "Deployment checklist here.\n\nAnd the deployment checklist again.",
            ),
            md("dst1", "ops/deploy.md", "Deployment checklist", ""),
        ];
        let got = find_mentions(&docs, NO_EDGES, 10);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].matched, "Deployment checklist");
    }

    #[test]
    fn html_and_memory_sources_are_reported_but_not_applicable() {
        let docs = [
            html(
                "mem1",
                "memories/a.html",
                "A memory",
                "Recorded while writing the Deployment checklist.",
                true,
            ),
            html(
                "art1",
                "research/b.html",
                "Some research",
                "Compare with the Deployment checklist.",
                false,
            ),
            md(
                "note1",
                "a/notes.md",
                "Notes about ops",
                "see the Deployment checklist",
            ),
            md("dst1", "ops/deploy.md", "Deployment checklist", ""),
        ];
        let got = find_mentions(&docs, NO_EDGES, 10);
        assert_eq!(got.len(), 3, "{got:?}");
        assert_eq!(got[0].src_id, "note1", "applicable rows sort first");
        assert!(got[0].applicability.note().is_none());
        let mem = got.iter().find(|m| m.src_id == "mem1").unwrap();
        assert!(!mem.applicability.is_applicable());
        assert_eq!(
            mem.applicability.note(),
            Some("cannot link from an HTML memory body — invariant #29")
        );
        let art = got.iter().find(|m| m.src_id == "art1").unwrap();
        assert!(!art.applicability.is_applicable());
        assert!(art.applicability.note().is_some_and(|n| n.contains("#29")));
    }

    #[test]
    fn memories_can_be_targets() {
        // The other half of the twice-declined ruling: a memory is a
        // perfectly good link DESTINATION.
        let docs = [
            md(
                "note1",
                "a/notes.md",
                "Notes about ops",
                "as recorded in Prod nofile ulimit outage",
            ),
            html(
                "mem1",
                "memories/ulimit.html",
                "Prod nofile ulimit outage",
                "",
                true,
            ),
        ];
        let got = find_mentions(&docs, NO_EDGES, 10);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].dst_id, "mem1");
        assert!(
            got[0].applicability.is_applicable(),
            "the SOURCE is the note"
        );
    }

    #[test]
    fn a_doc_with_an_empty_body_is_a_target_only() {
        // The apply path's contract: pass the whole corpus, fill in one body.
        let docs = [
            md(
                "src1",
                "a/notes.md",
                "Notes",
                "see the Deployment checklist",
            ),
            md(
                "src2",
                "b/other.md",
                "Other notes",
                "", // not scanned
            ),
            md("dst1", "ops/deploy.md", "Deployment checklist", ""),
        ];
        let got = find_mentions(&docs, NO_EDGES, 10);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].src_id, "src1");
    }

    #[test]
    fn limit_truncates_after_ordering() {
        let docs = [
            md("s1", "a/1.md", "One", "see the Deployment checklist"),
            md("s2", "a/2.md", "Two", "see the Deployment checklist"),
            md("dst1", "ops/deploy.md", "Deployment checklist", ""),
        ];
        let got = find_mentions(&docs, NO_EDGES, 1);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].src_id, "s1", "path order decides who survives");
        assert!(find_mentions(&docs, NO_EDGES, 0).is_empty());
    }

    #[test]
    fn target_never_uses_an_ambiguous_title() {
        // Two docs share a TITLE, so `[[Shared title xx]]` would resolve
        // Ambiguous and the edge hook would drop it. The mention matched the
        // (unique) basename, and the authored target must be a form that
        // resolves to exactly one doc.
        let docs = [
            md(
                "src1",
                "a/notes.md",
                "Notes",
                "documented in deployment-checklist.md",
            ),
            md("dst1", "ops/deployment-checklist.md", "Shared title xx", ""),
            md("dst2", "infra/other.md", "Shared title xx", ""),
        ];
        let got = find_mentions(&docs, NO_EDGES, 10);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].target, "deployment-checklist.md");
        let candidates: Vec<DocLite> = docs
            .iter()
            .map(|d| DocLite {
                id: d.id.clone(),
                rel_path: d.rel_path.clone(),
                title: d.title.clone(),
            })
            .collect();
        assert_eq!(
            links::resolve(&got[0].target, &candidates),
            Resolution::One("dst1".into()),
            "the authored target resolves the way the edge hook will"
        );
    }

    #[test]
    fn target_falls_back_when_the_matched_text_resolves_elsewhere() {
        // `dst1`'s TITLE is path-shaped and collides with a real file, so
        // both the matched text and the title resolve (via the ladder's
        // path tier) to the OTHER doc. The target must fall through to a
        // form that comes back to `dst1` — verified, never assumed.
        let docs = [
            md(
                "src1",
                "a/notes.md",
                "Notes",
                "documented in reports/2026-summary.md today",
            ),
            md(
                "dst1",
                "x/reports-summary.md",
                "reports/2026-summary.md",
                "",
            ),
            md(
                "other",
                "reports/2026-summary.md",
                "Something else entirely",
                "",
            ),
        ];
        let got = find_mentions(&docs, NO_EDGES, 10);
        let row = got
            .iter()
            .find(|m| m.dst_id == "dst1")
            .unwrap_or_else(|| panic!("{got:?}"));
        assert_eq!(row.matched, "reports/2026-summary.md");
        assert_eq!(row.target, "reports-summary");
        let candidates: Vec<DocLite> = docs
            .iter()
            .map(|d| DocLite {
                id: d.id.clone(),
                rel_path: d.rel_path.clone(),
                title: d.title.clone(),
            })
            .collect();
        assert_eq!(
            links::resolve(&row.target, &candidates),
            Resolution::One("dst1".into())
        );
    }

    #[test]
    fn matches_do_not_overlap() {
        // The longer name consumes the span, so the shorter one nested
        // inside it is not ALSO reported (noise, not a second thread).
        let docs = [
            md(
                "src1",
                "a/notes.md",
                "Notes",
                "documented in reports/2026-summary.md today",
            ),
            md("dst1", "x/y.md", "reports/2026-summary.md", ""),
            md("inner", "z/2026-summary.md", "Inner doc title", ""),
        ];
        let got = find_mentions(&docs, NO_EDGES, 10);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].dst_id, "dst1");
    }

    #[test]
    fn applicability_classifies_source_kinds() {
        assert_eq!(applicability(true, None), Applicability::Markdown);
        assert_eq!(
            applicability(true, Some("memory-user")),
            Applicability::Markdown,
            "a Markdown source is applicable whatever its category"
        );
        assert_eq!(
            applicability(false, Some("memory-user")),
            Applicability::MemoryHtml
        );
        assert_eq!(applicability(false, Some("note")), Applicability::Html);
        assert_eq!(applicability(false, None), Applicability::Html);
    }

    // --- apply -------------------------------------------------------------

    #[test]
    fn apply_writes_a_bare_wikilink_when_the_case_matches() {
        let body = "Read the Deployment checklist before shipping.\n";
        let got = apply_wikilink(body, "Deployment checklist", "Deployment checklist").unwrap();
        assert_eq!(got, "Read the [[Deployment checklist]] before shipping.\n");
    }

    #[test]
    fn apply_preserves_prose_case_with_an_alias() {
        let body = "we ran the deployment checklist twice\n";
        let got = apply_wikilink(body, "deployment checklist", "Deployment checklist").unwrap();
        assert_eq!(
            got,
            "we ran the [[Deployment checklist|deployment checklist]] twice\n"
        );
        // …and the authored link parses back with the right target.
        let parsed = links::parse_wikilinks(&got);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].target, "Deployment checklist");
        assert_eq!(parsed[0].alias.as_deref(), Some("deployment checklist"));
    }

    #[test]
    fn apply_skips_a_code_occurrence_and_links_the_prose_one() {
        // The verification loop's reason to exist: the first RAW occurrence
        // is inside a fence, so it can't be the one we link.
        let body = "```\nDeployment checklist\n```\n\nSee the Deployment checklist.\n";
        let got = apply_wikilink(body, "Deployment checklist", "Deployment checklist").unwrap();
        assert_eq!(
            got,
            "```\nDeployment checklist\n```\n\nSee the [[Deployment checklist]].\n"
        );
    }

    #[test]
    fn apply_refuses_when_the_mention_is_gone_or_only_in_code() {
        assert!(apply_wikilink(
            "nothing to see here\n",
            "Deployment checklist",
            "Deployment checklist"
        )
        .is_none());
        assert!(apply_wikilink(
            "`Deployment checklist` only\n",
            "Deployment checklist",
            "Deployment checklist"
        )
        .is_none());
    }

    #[test]
    fn apply_refuses_wikilink_syntax_in_either_text() {
        assert!(
            apply_wikilink("a Deployment checklist b", "Deployment checklist", "a|b").is_none()
        );
        assert!(apply_wikilink("a [[x]] b", "[[x]]", "Target name here").is_none());
    }

    #[test]
    fn apply_leaves_an_existing_link_label_alone() {
        // The only occurrence is a Markdown link's label — splicing there
        // would break the link it sits in, and `prose_text` skips link
        // labels, so the phrase has no prose occurrence to spend.
        let body = "see [Deployment checklist](https://example.test/) for more\n";
        assert!(apply_wikilink(body, "Deployment checklist", "Deployment checklist").is_none());
    }

    #[test]
    fn apply_round_trips_a_found_mention() {
        // Engine → apply → the pair is now edged, so the queue no longer
        // reports it (the loop the CLI drives).
        let src_body = "Read the Deployment checklist before shipping.\n";
        let docs = [
            md("src1", "ops/runbook.md", "Ops runbook", src_body),
            md("dst1", "ops/deploy.md", "Deployment checklist", ""),
        ];
        let m = find_mentions(&docs, NO_EDGES, 10).remove(0);
        let new_body = apply_wikilink(src_body, &m.matched, &m.target).unwrap();
        assert!(new_body.contains("[[Deployment checklist]]"));

        let docs = [
            md("src1", "ops/runbook.md", "Ops runbook", &new_body),
            md("dst1", "ops/deploy.md", "Deployment checklist", ""),
        ];
        assert!(
            find_mentions(&docs, NO_EDGES, 10).is_empty(),
            "the mention is now a link, so it is no longer unlinked"
        );
    }
}
