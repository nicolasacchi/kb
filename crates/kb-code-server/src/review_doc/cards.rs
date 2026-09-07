//! `kbc-review/1` CARDS — a ref resolved into a live, highlighted card
//! (V73-K1, design D9-a: "the agent's refs become live, syntax-highlighted
//! code cards rendered by kb-code").
//!
//! Computed PER REQUEST and persisted nowhere — `codelens/1`'s posture and
//! kb root invariant #2's "kb-code mints classes, nothing is cached",
//! applied to a citation. A card is a claim about the repository as it is
//! right now; storing one would create a second thing that can be stale in
//! a system whose whole honesty story is that a stale fact must never read
//! as a fresh one.
//!
//! # The three states, and the one rule behind them
//!
//! | state | what it means |
//! |---|---|
//! | `pinned` | the bytes the author cited are the bytes this card shows |
//! | `carried` | the bytes moved, and the ladder re-anchored them, with a caption saying how |
//! | `orphan` | no honest match — the position is NOT reported |
//!
//! (`gh:` and `kb:` carry `inert`: this daemon never calls GitHub and does
//! not own the kb corpus, so it makes no claim about them at all.)
//!
//! **A ref that does not resolve is an ORPHAN, never a guessed line.** That
//! is the same law `review_comments`'s module doc states for a comment
//! anchor, and it is enforced here by REUSING that ladder rather than
//! writing a second matcher: [`annotations::anchor_for_line`] builds a
//! `Selection` from the line the author cited IN THE BLOB THEY PINNED,
//! [`annotations::resolve`] re-anchors it against the target patchset's
//! blob, and [`review_comments::line_matches_snippet`] is the guard that
//! turns "resolve fell back to the original offset" into an orphan instead
//! of a wrong line.
//!
//! # Trust
//!
//! [`trust_for`] is the ONE minter, and `exact` is reachable from exactly
//! two shapes: a `code:` ref whose pinned blob IS the target blob (byte
//! equality — nothing to be wrong about), and a `finding:` ref, which
//! addresses a row in this daemon's own store. A carried ref is capped at
//! `likely` even when the ladder matched the snippet VERBATIM: an exact
//! text match at a different line in a different blob is strong evidence
//! that it is the same code, not proof, and this crate's oracle bar is that
//! a wrong `exact` is a release blocker (kb-code-server/CLAUDE.md
//! invariants 13/20).

use crate::annotations;
use crate::entities;
use crate::git::{GitRepo, DEFAULT_BLOB_SIZE_CAP};
use crate::highlight::Span;
use crate::ingest::git_blob_hash;
use crate::review_comments;
use crate::review_doc::refs::Ref;
use crate::store::{ReviewPatchsetRow, Store};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;

pub const STATE_PINNED: &str = "pinned";
pub const STATE_CARRIED: &str = "carried";
pub const STATE_ORPHAN: &str = "orphan";
pub const STATE_INERT: &str = "inert";

/// How many lines a card ever shows. A citation is an anchor, not a file
/// viewer; the reader clicks through for the rest.
pub const MAX_SNIPPET_LINES: u32 = 40;

/// What the whole ref set of one document resolves to.
#[derive(Debug, Clone, Serialize)]
pub struct Card {
    /// The ref body exactly as the author wrote it — the key everything
    /// else (lint rows, the SPA, the rendered export) joins on.
    pub r#ref: String,
    pub scheme: &'static str,
    /// `pinned` | `carried` | `orphan` | `inert`.
    pub state: &'static str,
    /// `exact` | `likely` | `candidate` — absent for an orphan (there is
    /// nothing to grade) and for an inert link (no claim is made).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trust: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_end: Option<u32>,
    /// The blob the REF pinned (`@sha`), verbatim as written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blob_sha: Option<String>,
    /// The blob that path has at the target patchset right now.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_blob: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    /// The first line number `snippet` shows (so a renderer can number the
    /// gutter without re-deriving it).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet_start: Option<u32>,
    /// Server-computed highlight spans, byte offsets REBASED onto
    /// `snippet`. `null` when the target blob is not the one this daemon
    /// has indexed — a pure store lookup, never derived inside a request
    /// handler (`GET /api/file`'s own rule).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub highlights: Option<Vec<Span>>,
    /// Always present, always the truth about what this card is: why it is
    /// pinned, how it was carried, or why it is an orphan.
    pub caption: String,
}

impl Card {
    fn orphan(r: &Ref, caption: impl Into<String>) -> Card {
        Card {
            r#ref: r.raw().to_string(),
            scheme: r.scheme(),
            state: STATE_ORPHAN,
            trust: None,
            path: None,
            line: None,
            line_end: None,
            blob_sha: None,
            current_blob: None,
            snippet: None,
            snippet_start: None,
            highlights: None,
            caption: caption.into(),
        }
    }

    fn base(r: &Ref, state: &'static str, trust: Option<&'static str>, caption: String) -> Card {
        Card {
            r#ref: r.raw().to_string(),
            scheme: r.scheme(),
            state,
            trust,
            path: None,
            line: None,
            line_end: None,
            blob_sha: None,
            current_blob: None,
            snippet: None,
            snippet_start: None,
            highlights: None,
            caption,
        }
    }
}

#[cfg(test)]
impl Card {
    /// Build an orphan card from a bare ref STRING — for the render/lint
    /// unit tests, which need a card shape without a repository behind it.
    pub fn orphan_for_test(r: &str, caption: &str) -> Card {
        Card {
            r#ref: r.to_string(),
            scheme: "code",
            state: STATE_ORPHAN,
            trust: None,
            path: None,
            line: None,
            line_end: None,
            blob_sha: None,
            current_blob: None,
            snippet: None,
            snippet_start: None,
            highlights: None,
            caption: caption.to_string(),
        }
    }
}

/// The ONE trust minter. See the module doc for why `carried` is capped at
/// `likely` no matter how good the textual match was.
pub fn trust_for(state: &str, byte_identical: bool) -> Option<&'static str> {
    match state {
        STATE_PINNED if byte_identical => Some("exact"),
        STATE_PINNED => Some("likely"),
        STATE_CARRIED => Some("likely"),
        _ => None,
    }
}

/// Everything a resolution pass needs that is not the store.
pub struct CardCtx<'a> {
    pub repo_root: &'a Path,
    pub repo_id: i64,
    pub review_id: i64,
    pub target_ps: &'a ReviewPatchsetRow,
    /// Every patchset number this review actually has — so a `hunk:` ref
    /// naming one it does not can say so by name.
    pub known_ps: &'a HashSet<i64>,
    /// The paths the target patchset changes — a `hunk:` ref's own
    /// existence check.
    pub changed_paths: &'a HashSet<String>,
    /// V73-K3 — the review's PSEUDO-FILES (`kbc-pseudo/1`), when the
    /// caller built them. A `code:` ref whose path is `~review/<name>`
    /// resolves against these bytes instead of the tree, through the SAME
    /// ladder and with the same states: a pseudo-file carries a real git
    /// blob hash, so `@sha` byte-equality means exactly what it means for
    /// a tracked file. `None` (the pre-K3 shape) simply means no `~review/`
    /// path can resolve, and such a ref is an ordinary orphan.
    pub pseudo: Option<&'a crate::review_pseudo::PseudoSet>,
}

/// Resolve every ref into a card, in the order given. One blob read per
/// distinct `(path, sha)` for the whole pass (the same per-request caching
/// convention `review_comments::build_comment_groups` uses).
pub fn resolve_cards(store: &Store, ctx: &CardCtx<'_>, refs: &[Ref]) -> Vec<Card> {
    let mut cache: HashMap<String, Option<String>> = HashMap::new();
    let mut oid_cache: HashMap<String, Option<String>> = HashMap::new();
    refs.iter()
        .map(|r| resolve_one(store, ctx, r, &mut cache, &mut oid_cache))
        .collect()
}

fn resolve_one(
    store: &Store,
    ctx: &CardCtx<'_>,
    r: &Ref,
    cache: &mut HashMap<String, Option<String>>,
    oid_cache: &mut HashMap<String, Option<String>>,
) -> Card {
    match r {
        Ref::Gh { kind, id, .. } => Card::base(
            r,
            STATE_INERT,
            None,
            format!("GitHub {kind} {id} — kb-code never calls GitHub; this is a link, not a claim"),
        ),
        Ref::Kb { kb, id, .. } => Card::base(
            r,
            STATE_INERT,
            None,
            format!("kb corpus {kb}, document {id} — kb-code does not own this corpus; this is a link, not a claim"),
        ),
        Ref::Finding { slug, .. } => resolve_finding(store, ctx, r, slug),
        Ref::Hunk { path, ps, index, .. } => resolve_hunk(ctx, r, path, *ps, *index),
        Ref::Ent { fqn, .. } => resolve_ent(store, ctx, r, fqn, cache, oid_cache),
        Ref::Sym {
            container, name, ..
        } => resolve_sym(store, ctx, r, container.as_deref(), name, cache, oid_cache),
        Ref::Code {
            path,
            line,
            line_end,
            sha,
            ..
        } => resolve_code(
            store,
            ctx,
            r,
            path,
            *line,
            *line_end,
            sha.as_deref(),
            cache,
            oid_cache,
        ),
    }
}

// --- code ------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn resolve_code(
    store: &Store,
    ctx: &CardCtx<'_>,
    r: &Ref,
    path: &str,
    line: Option<u32>,
    line_end: Option<u32>,
    sha: Option<&str>,
    cache: &mut HashMap<String, Option<String>>,
    oid_cache: &mut HashMap<String, Option<String>>,
) -> Card {
    // V73-K3 — a `~review/…` path addresses a review PSEUDO-FILE. Same
    // ladder, different source of bytes: the content is rendered rather
    // than read out of the tree, and its `blob_sha` is a real git blob
    // hash of those bytes, so `pinned`/`carried`/`orphan` and
    // `trust_for`'s byte-equality rule all apply unchanged.
    if let Some(name) = crate::review_pseudo::name_for_path(path) {
        return match ctx.pseudo.and_then(|set| set.get(name)) {
            Some(file) if file.present => {
                pseudo_code_card(store, r, path, file, line, line_end, sha)
            }
            Some(file) => Card::orphan(
                r,
                format!(
                    "{path} is a review pseudo-file with no content: {}",
                    file.reason.as_deref().unwrap_or("nothing to render")
                ),
            ),
            None => Card::orphan(
                r,
                format!(
                    "{path} names a review pseudo-file, but this read did not build them                      (pseudo-files are per-review; a ref to one only resolves inside its own                      review's document)"
                ),
            ),
        };
    }

    let tip = ctx.target_ps.tip_sha.as_str();
    let Some(content) = read_at_ps(ctx.repo_root, path, tip, cache) else {
        return Card::orphan(
            r,
            format!(
                "{path} does not exist (or is not UTF-8 text) at patchset {}",
                ctx.target_ps.ps_number
            ),
        );
    };
    let current_blob = blob_oid_at_ps(ctx.repo_root, path, tip, oid_cache)
        .unwrap_or_else(|| git_blob_hash(content.as_bytes()));
    let total = content.lines().count() as u32;

    // No `@sha`: the author made no claim about WHICH bytes, so neither do
    // we — the range is read from the target patchset as it stands now, and
    // the caption says exactly that.
    let Some(sha) = sha else {
        let Some(start) = line else {
            return finish_code(
                store,
                r,
                path,
                &content,
                &current_blob,
                None,
                1,
                total.max(1),
                STATE_PINNED,
                format!(
                    "whole file at patchset {} (the ref pins no blob)",
                    ctx.target_ps.ps_number
                ),
            );
        };
        if start == 0 || start > total {
            return Card::orphan(
                r,
                format!(
                    "line {start} is outside {path} ({total} lines at patchset {})",
                    ctx.target_ps.ps_number
                ),
            );
        }
        let end = line_end.unwrap_or(start).min(total);
        return finish_code(
            store,
            r,
            path,
            &content,
            &current_blob,
            None,
            start,
            end,
            STATE_PINNED,
            format!(
                "the ref pins no blob — shown as {path} reads at patchset {}",
                ctx.target_ps.ps_number
            ),
        );
    };

    // The author pinned a blob. Byte equality is the only thing that can
    // mint `exact` here.
    if current_blob.starts_with(sha) {
        let (start, end) = match line {
            None => (1, total.max(1)),
            Some(s) if s == 0 || s > total => {
                return Card::orphan(
                    r,
                    format!("line {s} is outside {path} ({total} lines) even though the blob matches — the ref's own line number is wrong"),
                )
            }
            Some(s) => (s, line_end.unwrap_or(s).min(total)),
        };
        return finish_code(
            store,
            r,
            path,
            &content,
            &current_blob,
            Some(sha.to_string()),
            start,
            end,
            STATE_PINNED,
            format!(
                "blob {sha} is still the blob at patchset {}",
                ctx.target_ps.ps_number
            ),
        );
    }

    // The blob moved. Carry forward, or orphan honestly.
    let Some(line) = line else {
        return Card::orphan(
            r,
            format!(
                "{path} is blob {} at patchset {}, not the pinned {sha}, and the ref cites no line to carry forward",
                short(&current_blob),
                ctx.target_ps.ps_number
            ),
        );
    };
    let old = match GitRepo::open(ctx.repo_root)
        .and_then(|g| g.read_blob_by_oid(sha, DEFAULT_BLOB_SIZE_CAP))
    {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(t) => t,
            Err(_) => {
                return Card::orphan(r, format!("the pinned blob {sha} is not UTF-8 text"));
            }
        },
        Err(e) => {
            return Card::orphan(
                r,
                format!("the pinned blob {sha} is not readable from this repository: {e}"),
            );
        }
    };
    let Some(old_text) = old.lines().nth(line.saturating_sub(1) as usize) else {
        return Card::orphan(
            r,
            format!("line {line} does not exist in the pinned blob {sha}"),
        );
    };
    let anchor = annotations::anchor_for_line(line, old_text);
    let resolved = annotations::resolve(&content, &anchor);
    let snippet = match &anchor {
        kb_core::review::Anchor::Selection { snippet, .. } => snippet.as_str(),
        _ => "",
    };
    if resolved.stale || !review_comments::line_matches_snippet(&content, resolved.line, snippet) {
        return Card::orphan(
            r,
            format!(
                "line {line} of blob {sha} has no honest match in {path} at patchset {} — a wrong line is worse than an orphan",
                ctx.target_ps.ps_number
            ),
        );
    }
    let shift = resolved.line as i64 - line as i64;
    let start = resolved.line;
    let end = match line_end {
        Some(e) => ((e as i64 + shift).max(start as i64) as u32).min(total),
        None => start,
    };
    let how = match resolved.confidence {
        annotations::MatchConfidence::Exact => "exact",
        annotations::MatchConfidence::Fuzzy => "fuzzy",
    };
    finish_code(
                store, r,
        path,
        &content,
        &current_blob,
        Some(sha.to_string()),
        start,
        end,
        STATE_CARRIED,
        format!(
            "carried from blob {sha} line {line} to line {start} ({how} snippet match); {path} is blob {} at patchset {}",
            short(&current_blob),
            ctx.target_ps.ps_number
        ),
    )
}

#[allow(clippy::too_many_arguments)]
/// A `code:` card over a PSEUDO-FILE's rendered bytes. There is no
/// carry-forward rung here on purpose: a pseudo-file is regenerated whole
/// on every read, so "the same line, moved" is not a thing that happened —
/// either the author pinned the bytes that are still current (`pinned`,
/// `exact`) or they pinned bytes that no longer exist, and re-anchoring
/// prose into a regenerated document would be a guess with nothing behind
/// it (`orphan`). A ref with no `@sha` is `pinned` at `likely`, exactly as
/// it is for a tracked file: the author claimed nothing about which bytes.
fn pseudo_code_card(
    store: &Store,
    r: &Ref,
    path: &str,
    file: &crate::review_pseudo::PseudoFile,
    line: Option<u32>,
    line_end: Option<u32>,
    sha: Option<&str>,
) -> Card {
    let content = file.content();
    let total = content.lines().count().max(1) as u32;
    let start = line.unwrap_or(1).clamp(1, total);
    let end = line_end.unwrap_or(start).clamp(start, total);
    match sha {
        None => finish_code(
            store,
            r,
            path,
            content,
            &file.blob_sha,
            None,
            start,
            end,
            STATE_PINNED,
            format!(
                "{path} is a review pseudo-file rendered from {}; this ref pinned no blob, so                  the lines shown are the CURRENT ones",
                file.source
            ),
        ),
        Some(sha) if file.blob_sha.starts_with(sha) => finish_code(
            store,
            r,
            path,
            content,
            &file.blob_sha,
            Some(sha.to_string()),
            start,
            end,
            STATE_PINNED,
            format!(
                "the bytes this ref cites are still {path}'s own ({})",
                file.blob_sha
            ),
        ),
        Some(sha) => Card::orphan(
            r,
            format!(
                "this ref cites {sha} of {path}, which now renders as {} — a pseudo-file is                  regenerated whole on every read, so there is no moved line to carry the ref                  forward to",
                file.blob_sha
            ),
        ),
    }
}

fn finish_code(
    store: &Store,
    r: &Ref,
    path: &str,
    content: &str,
    current_blob: &str,
    blob_sha: Option<String>,
    start: u32,
    end: u32,
    state: &'static str,
    caption: String,
) -> Card {
    let byte_identical = state == STATE_PINNED
        && blob_sha
            .as_deref()
            .is_some_and(|s| current_blob.starts_with(s));
    let (snippet, snippet_start, byte_range) = slice_snippet(content, start, end);
    let highlights = highlights_for(store, path, content, current_blob, byte_range);
    Card {
        r#ref: r.raw().to_string(),
        scheme: r.scheme(),
        state,
        trust: trust_for(state, byte_identical),
        path: Some(path.to_string()),
        line: Some(start),
        line_end: if end > start { Some(end) } else { None },
        blob_sha,
        current_blob: Some(current_blob.to_string()),
        snippet: Some(snippet),
        snippet_start: Some(snippet_start),
        highlights,
        caption,
    }
}

// --- sym / ent --------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn resolve_sym(
    store: &Store,
    ctx: &CardCtx<'_>,
    r: &Ref,
    container: Option<&str>,
    name: &str,
    cache: &mut HashMap<String, Option<String>>,
    oid_cache: &mut HashMap<String, Option<String>>,
) -> Card {
    // The EXACT rung of `symbol_addr`'s own ladder, and only that rung: a
    // ref card is a CITATION with no fallback anchor beside it, so the
    // route's fuzzy rung (which exists because `?sym=` links always carry a
    // path/line fallback) would be a guess presented as a citation.
    let rows = match store.symbols_named_in_repo(ctx.repo_id, name) {
        Ok(rows) => rows,
        Err(e) => return Card::orphan(r, format!("symbol lookup failed: {e}")),
    };
    let matches: Vec<_> = rows
        .into_iter()
        .filter(|(_, s)| match container {
            Some(c) => s.container.as_deref() == Some(c),
            None => true,
        })
        .collect();
    if matches.is_empty() {
        return Card::orphan(
            r,
            match container {
                Some(c) => format!("no indexed symbol {c}::{name} in this repository"),
                None => format!("no indexed symbol named {name:?} in this repository"),
            },
        );
    }
    if matches.len() > 1 {
        return Card::orphan(
            r,
            format!(
                "{} indexed symbols match — a ref names one thing, so this is an orphan rather than a guess (qualify it: sym:Container#{name})",
                matches.len()
            ),
        );
    }
    let (path, sym) = &matches[0];
    positioned_card(
        store,
        ctx,
        r,
        path,
        sym.line_start,
        sym.line_end,
        None,
        format!(
            "symbol {}{} in the index",
            container.map(|c| format!("{c}::")).unwrap_or_default(),
            name
        ),
        cache,
        oid_cache,
    )
}

#[allow(clippy::too_many_arguments)]
fn resolve_ent(
    store: &Store,
    ctx: &CardCtx<'_>,
    r: &Ref,
    fqn: &str,
    cache: &mut HashMap<String, Option<String>>,
    oid_cache: &mut HashMap<String, Option<String>>,
) -> Card {
    if entities::validate_ent(fqn).is_err() {
        return Card::orphan(
            r,
            format!("{fqn:?} is not an address the entity index answers (a Ruby constant path)"),
        );
    }
    let rows =
        match store.entity_defs_for_name(ctx.repo_id, None, fqn, entities::MAX_DEFS_PER_QUERY) {
            Ok(rows) => rows,
            Err(e) => return Card::orphan(r, format!("entity lookup failed: {e}")),
        };
    if rows.is_empty() {
        return Card::orphan(
            r,
            format!("no entity definition site for {fqn} in the index"),
        );
    }
    if rows.len() > 1 {
        return Card::orphan(
            r,
            format!(
                "{fqn} has {} definition sites — a ref names one thing, so this is an orphan rather than a guess",
                rows.len()
            ),
        );
    }
    let row = &rows[0];
    // Borrow the entity index's OWN class; never mint one here
    // (kb-code-server/CLAUDE.md invariant 13).
    let zw = entities::zeitwerk::zeitwerk_for(ctx.repo_root);
    let matched_via = if row.fqn == fqn {
        entities::MATCHED_VIA_NESTING
    } else {
        entities::MATCHED_VIA_ZEITWERK
    };
    let drifted = false; // the blob check below is the card's own drift signal
    let ent_class = entities::class_for(matched_via, &row.nesting, zw.state, drifted);
    let mut card = positioned_card(
        store,
        ctx,
        r,
        &row.path,
        row.line_start as u32,
        row.line_end as u32,
        Some(ent_class),
        format!("{fqn} ({}), entity index class {ent_class}", row.kind),
        cache,
        oid_cache,
    );
    // The entity's own class is a CEILING on this card's trust: a card can
    // never be more certain about where a constant lives than the index it
    // borrowed the answer from.
    card.trust = cap_trust(card.trust, ent_class);
    card
}

/// The weaker of two classes on the `exact > likely > candidate` order.
fn cap_trust(a: Option<&'static str>, ceiling: &'static str) -> Option<&'static str> {
    fn rank(s: &str) -> u8 {
        match s {
            "exact" => 3,
            "likely" => 2,
            "candidate" => 1,
            _ => 0,
        }
    }
    let a = a?;
    Some(if rank(ceiling) < rank(a) { ceiling } else { a })
}

/// A card for something the INDEX places at `path:line` — a symbol or an
/// entity. The index tracks the WORKING TREE, so the card is `pinned` only
/// when the indexed blob is also the target patchset's blob; otherwise the
/// position is honestly `carried` (the definition is where the index says
/// in the checkout, which is not the blob under review).
#[allow(clippy::too_many_arguments)]
fn positioned_card(
    store: &Store,
    ctx: &CardCtx<'_>,
    r: &Ref,
    path: &str,
    line_start: u32,
    line_end: u32,
    ceiling: Option<&'static str>,
    what: String,
    cache: &mut HashMap<String, Option<String>>,
    oid_cache: &mut HashMap<String, Option<String>>,
) -> Card {
    let tip = ctx.target_ps.tip_sha.as_str();
    let indexed_blob = store
        .get_file(ctx.repo_id, path)
        .ok()
        .flatten()
        .map(|f| f.blob_hash);
    let Some(content) = read_at_ps(ctx.repo_root, path, tip, cache) else {
        return Card::orphan(
            r,
            format!(
                "{what}: {path} does not exist (or is not UTF-8 text) at patchset {}",
                ctx.target_ps.ps_number
            ),
        );
    };
    let current_blob = blob_oid_at_ps(ctx.repo_root, path, tip, oid_cache)
        .unwrap_or_else(|| git_blob_hash(content.as_bytes()));
    let same = indexed_blob.as_deref() == Some(current_blob.as_str());
    let total = content.lines().count() as u32;
    if line_start == 0 || line_start > total {
        return Card::orphan(
            r,
            format!(
                "{what}: the index places it at {path}:{line_start}, which is outside that file at patchset {} ({total} lines)",
                ctx.target_ps.ps_number
            ),
        );
    }
    let state = if same { STATE_PINNED } else { STATE_CARRIED };
    let end = line_end.max(line_start).min(total);
    let (snippet, snippet_start, byte_range) = slice_snippet(&content, line_start, end);
    let highlights = highlights_for(store, path, &content, &current_blob, byte_range);
    let caption =
        if same {
            format!(
                "{what} — the indexed blob IS patchset {}'s blob for {path}",
                ctx.target_ps.ps_number
            )
        } else {
            format!(
            "{what} — position comes from the INDEXED checkout ({}), not patchset {}'s blob ({})",
            indexed_blob.as_deref().map(short).unwrap_or_else(|| "not indexed".into()),
            ctx.target_ps.ps_number,
            short(&current_blob),
        )
        };
    let mut trust = trust_for(state, same);
    if let Some(c) = ceiling {
        trust = cap_trust(trust, c);
    }
    Card {
        r#ref: r.raw().to_string(),
        scheme: r.scheme(),
        state,
        trust,
        path: Some(path.to_string()),
        line: Some(line_start),
        line_end: if end > line_start { Some(end) } else { None },
        blob_sha: indexed_blob,
        current_blob: Some(current_blob),
        snippet: Some(snippet),
        snippet_start: Some(snippet_start),
        highlights,
        caption,
    }
}

// --- finding / hunk ---------------------------------------------------------

fn resolve_finding(store: &Store, ctx: &CardCtx<'_>, r: &Ref, slug: &str) -> Card {
    match store.get_review_finding(ctx.review_id, slug) {
        Ok(Some(row)) => {
            let mut card = Card::base(
                r,
                STATE_PINNED,
                Some("exact"),
                format!(
                    "finding {slug} — {} / {} at {}{}",
                    row.act,
                    row.severity,
                    row.location_path,
                    if row.superseded { " (superseded)" } else { "" }
                ),
            );
            card.path = Some(row.location_path.clone());
            card.line = first_location_line(row.location_lines.as_deref());
            card
        }
        Ok(None) => Card::orphan(r, format!("review {} has no finding {slug}", ctx.review_id)),
        Err(e) => Card::orphan(r, format!("finding lookup failed: {e}")),
    }
}

fn first_location_line(lines_json: Option<&str>) -> Option<u32> {
    let v: serde_json::Value = serde_json::from_str(lines_json?).ok()?;
    v.as_array()?.first()?.as_i64().map(|n| n as u32)
}

fn resolve_hunk(ctx: &CardCtx<'_>, r: &Ref, path: &str, ps: i64, index: u32) -> Card {
    if !ctx.known_ps.contains(&ps) {
        return Card::orphan(
            r,
            format!(
                "review {} has no patchset {ps} (it has {})",
                ctx.review_id,
                sorted_list(ctx.known_ps)
            ),
        );
    }
    if ps != ctx.target_ps.ps_number {
        return Card::orphan(
            r,
            format!(
                "the ref addresses patchset {ps}; this read resolved against patchset {} — re-read with ?ps={ps}",
                ctx.target_ps.ps_number
            ),
        );
    }
    if !ctx.changed_paths.contains(path) {
        return Card::orphan(r, format!("patchset {ps} does not change {path}"));
    }
    let mut card = Card::base(
        r,
        STATE_PINNED,
        Some("likely"),
        format!(
            "{path} is changed by patchset {ps}; hunk index {index} is NOT verified against the diff in this milestone (diff v2 owns hunk addressing)"
        ),
    );
    card.path = Some(path.to_string());
    card
}

fn sorted_list(set: &HashSet<i64>) -> String {
    let mut v: Vec<i64> = set.iter().copied().collect();
    v.sort_unstable();
    v.iter().map(i64::to_string).collect::<Vec<_>>().join(", ")
}

// --- shared helpers ---------------------------------------------------------

fn read_at_ps(
    repo_root: &Path,
    path: &str,
    tip: &str,
    cache: &mut HashMap<String, Option<String>>,
) -> Option<String> {
    if let Some(hit) = cache.get(path) {
        return hit.clone();
    }
    let text = review_comments::read_blob_text(repo_root, path, tip);
    cache.insert(path.to_string(), text.clone());
    text
}

fn blob_oid_at_ps(
    repo_root: &Path,
    path: &str,
    tip: &str,
    cache: &mut HashMap<String, Option<String>>,
) -> Option<String> {
    if let Some(hit) = cache.get(path) {
        return hit.clone();
    }
    let oid = GitRepo::open(repo_root)
        .ok()
        .and_then(|g| g.blob_oid(tip, path).ok())
        .flatten();
    cache.insert(path.to_string(), oid.clone());
    oid
}

/// Lines `[start, end]` (1-based, inclusive) of `content`, capped at
/// [`MAX_SNIPPET_LINES`], plus the byte range the slice occupies in
/// `content` (for rebasing highlight spans).
fn slice_snippet(content: &str, start: u32, end: u32) -> (String, u32, (usize, usize)) {
    let start = start.max(1);
    let end = end.max(start).min(start + MAX_SNIPPET_LINES - 1);
    let mut byte_start = 0usize;
    let mut byte_end = content.len();
    let mut out = String::new();
    let mut pos = 0usize;
    for (i, line) in content.split_inclusive('\n').enumerate() {
        let n = (i + 1) as u32;
        if n == start {
            byte_start = pos;
        }
        if n >= start && n <= end {
            out.push_str(line);
        }
        pos += line.len();
        if n == end {
            byte_end = pos;
            break;
        }
    }
    if byte_end < byte_start {
        byte_end = byte_start;
    }
    (out, start, (byte_start, byte_end))
}

/// Highlight spans for the snippet, or `None`.
///
/// A pure STORE lookup keyed on the blob's content hash and the language's
/// current salt — never derived on the spot, the same rule `GET /api/file`
/// spells out (deriving inside a request handler would either skip
/// persisting or corrupt the `files` working-tree invariant). A blob this
/// daemon has not indexed therefore honestly answers `null` rather than a
/// half-highlighted approximation.
fn highlights_for(
    store: &Store,
    path: &str,
    content: &str,
    current_blob: &str,
    (byte_start, byte_end): (usize, usize),
) -> Option<Vec<Span>> {
    let li = crate::lang::detect(path, Some(content.as_bytes()))?;
    let all = store.highlights_for_blob(current_blob, li.salt).ok()??;
    Some(clip_spans(&all, byte_start, byte_end))
}

/// Clip whole-file spans to `[byte_start, byte_end)` and rebase their
/// offsets onto the slice.
pub fn clip_spans(spans: &[Span], byte_start: usize, byte_end: usize) -> Vec<Span> {
    let mut out = Vec::new();
    for s in spans {
        let a = s.byte_start as usize;
        let b = a + s.byte_len as usize;
        let lo = a.max(byte_start);
        let hi = b.min(byte_end);
        if lo >= hi {
            continue;
        }
        out.push(Span {
            byte_start: (lo - byte_start) as u32,
            byte_len: (hi - lo) as u32,
            class: s.class,
        });
    }
    out
}

fn short(sha: &str) -> String {
    sha.chars().take(12).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::highlight::HighlightClass;

    #[test]
    fn a_carried_ref_is_never_exact_no_matter_how_good_the_match_was() {
        assert_eq!(trust_for(STATE_CARRIED, true), Some("likely"));
        assert_eq!(trust_for(STATE_CARRIED, false), Some("likely"));
        assert_eq!(trust_for(STATE_PINNED, true), Some("exact"));
        assert_eq!(
            trust_for(STATE_PINNED, false),
            Some("likely"),
            "a pin with no blob claim is not proof"
        );
        assert_eq!(trust_for(STATE_ORPHAN, true), None);
        assert_eq!(trust_for(STATE_INERT, true), None);
    }

    #[test]
    fn a_borrowed_class_is_a_ceiling_never_a_floor() {
        assert_eq!(cap_trust(Some("exact"), "likely"), Some("likely"));
        assert_eq!(cap_trust(Some("likely"), "exact"), Some("likely"));
        assert_eq!(cap_trust(Some("likely"), "candidate"), Some("candidate"));
        assert_eq!(cap_trust(None, "exact"), None);
    }

    #[test]
    fn snippet_slicing_is_inclusive_capped_and_byte_accurate() {
        let content = "a\nbb\nccc\ndddd\n";
        let (s, start, (lo, hi)) = slice_snippet(content, 2, 3);
        assert_eq!(s, "bb\nccc\n");
        assert_eq!(start, 2);
        assert_eq!(&content[lo..hi], "bb\nccc\n");

        let long: String = (1..=100).map(|n| format!("line {n}\n")).collect();
        let (s, _, _) = slice_snippet(&long, 1, 100);
        assert_eq!(
            s.lines().count(),
            MAX_SNIPPET_LINES as usize,
            "a card is an anchor, not a file viewer"
        );
    }

    #[test]
    fn spans_are_clipped_and_rebased_onto_the_snippet() {
        let spans = vec![
            Span {
                byte_start: 0,
                byte_len: 2,
                class: HighlightClass::Keyword,
            },
            Span {
                byte_start: 3,
                byte_len: 4,
                class: HighlightClass::String,
            },
            Span {
                byte_start: 20,
                byte_len: 2,
                class: HighlightClass::Comment,
            },
        ];
        // [0,2) is fully before the window and drops; [3,7) survives,
        // rebased to [1,5); [20,22) is fully after and drops.
        assert_eq!(
            clip_spans(&spans, 2, 8),
            vec![Span {
                byte_start: 1,
                byte_len: 4,
                class: HighlightClass::String
            }]
        );
    }
}
