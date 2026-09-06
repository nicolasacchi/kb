//! `links` — the wikilink / backlink surface that makes notes the connective
//! tissue of the corpus. Three read endpoints over the existing `edges` graph
//! (invariant: wikilinks ride `kind="link"` edges, recorded by the indexer's
//! edge-record hook from a note's `[[…]]` source):
//!
//! ```text
//! GET /api/kb/{kb}/backlinks/{id}        → { backlinks }   (any artifact)
//! GET /api/kb/{kb}/notes/{id}/links      → { outgoing, backlinks }  (one note)
//! GET /api/kb/{kb}/wikilinks/suggest?q=  → { suggestions }  ([[ autocomplete)
//! GET /api/kb/{kb}/links/suggest?limit=  → { suggestions }  (CT-F3 unlinked)
//! POST /api/kb/{kb}/links/apply          → author one suggested wikilink
//! ```
//!
//! `outgoing` is computed by re-parsing the note body and resolving each
//! `[[target]]` against the corpus (`kb_core::links::resolve`) — the SAME
//! resolution the edge hook used, so the rendered link and the recorded edge
//! agree. `backlinks` is the reverse-edge query (`backlinks_of`). Resolution
//! is corpus-local (edges are intra-kb), so no cross-kb fan-out here.
//!
//! CT-F3's pair (`links/suggest` + `links/apply`) is the unlinked-mention
//! queue: a DERIVED read-time report of docs whose prose names another
//! artifact with no edge to show for it, plus the one explicit,
//! human-driven verb that turns a row into a real `[[wikilink]]`. Nothing
//! is persisted by the report and nothing runs on ingest — see
//! `kb_core::mentions` for the engine and the invariant #29 ruling it
//! respects.

use crate::middleware::error_to_problem_json;
use crate::state::{KbContext, KbHandles, LinkCandidate, LinksCache, LinksIndex};
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::Response,
    response::IntoResponse,
    Json,
};
use kb_core::links::{self, DocLite, Resolution};
use kb_core::paths::{doc_folder, doc_rel_path};
use kb_core::storage::lance::DocSummary;
use kb_core::types::KbName;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// One outgoing wikilink from a note, resolved against the corpus. `target`
/// is the raw destination as written (the SPA's render-map key); `state`
/// distinguishes a clean hit from a dangling/ambiguous one so the UI can
/// style it (a dangling `[[…]]` is a "not yet created" affordance, never an
/// error).
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ResolvedLink {
    pub target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub alias: Option<String>,
    /// `"resolved"` | `"dangling"` | `"ambiguous"`.
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub id: Option<String>,
    pub kb: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub source_relative: Option<String>,
    pub is_note: bool,
}

/// One inbound reference — an artifact (often a note) that links *here*.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct BacklinkRef {
    pub id: String,
    pub kb: String,
    pub title: String,
    pub source_relative: String,
    pub folder: String,
    pub is_note: bool,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct BacklinksResponse {
    pub backlinks: Vec<BacklinkRef>,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct NoteLinks {
    pub outgoing: Vec<ResolvedLink>,
    pub backlinks: Vec<BacklinkRef>,
}

/// A `[[` autocomplete candidate.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WikilinkSuggestion {
    pub id: String,
    pub title: String,
    pub source_relative: String,
    pub is_note: bool,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct SuggestResponse {
    pub suggestions: Vec<WikilinkSuggestion>,
}

#[derive(Debug, Deserialize)]
pub struct SuggestQuery {
    pub q: String,
    #[serde(default)]
    pub limit: Option<usize>,
}

const SUGGEST_DEFAULT_LIMIT: usize = 12;
const SUGGEST_MAX_LIMIT: usize = 50;

/// Wave-2 — the corpus wikilink index for `ctx`, served from the
/// per-(kb, index-generation) memo when warm. On a generation change it
/// borrows the gallery memo's one `list_docs(u32::MAX)` scan (so the two
/// paths share a scan) and precomputes each doc's lowercased title/basename
/// ranking keys + its `DocLite` resolution candidate once, instead of the
/// suggest handler re-scanning + re-lowercasing the corpus per keystroke
/// and note-link resolution re-scanning + re-cloning it per note view.
///
/// Correctness mirrors `gallery_snapshot` (invariant #15): the `std::sync::
/// Mutex` guard is dropped before every `.await`, and a cache hit requires
/// `stored.generation == index_generation()` (monotonic), so a mutation that
/// bumps the generation mid-build only forces the next request to rebuild —
/// never a stale serve.
pub(crate) async fn links_index(ctx: &KbContext) -> Result<Arc<LinksIndex>, kb_core::Error> {
    let generation = ctx.storage.index_generation();
    {
        let guard = ctx.links_cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = guard.as_ref() {
            if c.generation == generation {
                return Ok(Arc::clone(&c.index));
            }
        }
    } // release the std Mutex BEFORE the await below — never held across .await

    let (rows, _edge_counts) = crate::routes::docs::gallery_snapshot(ctx).await?;
    let mut entries: Vec<LinkCandidate> = Vec::with_capacity(rows.len());
    let mut candidates: Vec<DocLite> = Vec::with_capacity(rows.len());
    for row in rows.iter() {
        let d = &row.doc;
        let source_relative = rel_of(d, &ctx.source_path);
        let basename_lower = source_relative
            .rsplit('/')
            .next()
            .unwrap_or(&source_relative)
            .to_lowercase();
        candidates.push(DocLite {
            id: d.id.clone(),
            rel_path: source_relative.clone(),
            title: d.title.clone(),
        });
        entries.push(LinkCandidate {
            title_lower: d.title.to_lowercase(),
            basename_lower,
            is_note: kb_core::notes::is_note(&d.path, d.kb_category.as_deref()),
            id: d.id.clone(),
            title: d.title.clone(),
            source_relative,
        });
    }
    let index = Arc::new(LinksIndex {
        entries,
        candidates,
    });

    {
        let mut guard = ctx.links_cache.lock().unwrap_or_else(|e| e.into_inner());
        *guard = Some(LinksCache {
            generation,
            index: Arc::clone(&index),
        });
    }
    Ok(index)
}

fn rel_of(doc: &DocSummary, source_root: &std::path::Path) -> String {
    let r = doc_rel_path(&doc.path, source_root);
    if r.is_empty() {
        doc.path.clone()
    } else {
        r
    }
}

/// Resolve every distinct `[[target]]` in `body` against the memoised
/// corpus index (see [`links_index`]). Pure aside from the supplied index;
/// the resolver itself is LLM-free. A self-link (target resolves to
/// `self_id`) is still reported as `resolved` so the body renders it, but
/// it never becomes an edge (the hook drops it).
pub(crate) fn resolve_outgoing(
    kb_name: &KbName,
    body: &str,
    index: &LinksIndex,
) -> Vec<ResolvedLink> {
    let parsed = links::parse_wikilinks(body);
    if parsed.is_empty() {
        return Vec::new();
    }
    // One resolver over the memoised candidate set — the ladder's lookup
    // tables are built once per call, then every target is hashmap work
    // (mirrors the edge-record hook's one-ResolveIndex-per-note shape).
    let resolver = links::ResolveIndex::new(&index.candidates);
    // Metadata lookup for a resolved id — one map build instead of an
    // O(corpus) linear `.find` per resolved link.
    let by_id: std::collections::HashMap<&str, &LinkCandidate> =
        index.entries.iter().map(|e| (e.id.as_str(), e)).collect();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out = Vec::new();
    for link in &parsed {
        let key = links::normalize_target(&link.target);
        if !seen.insert(key.clone()) {
            continue;
        }
        let kb = kb_name.as_str().to_string();
        let resolved = match resolver.resolve(&key) {
            Resolution::One(id) => {
                let entry = by_id.get(id.as_str()).copied();
                ResolvedLink {
                    target: link.target.clone(),
                    alias: link.alias.clone(),
                    state: "resolved".into(),
                    title: entry.map(|e| e.title.clone()),
                    source_relative: entry.map(|e| e.source_relative.clone()),
                    is_note: entry.map(|e| e.is_note).unwrap_or(false),
                    id: Some(id),
                    kb,
                }
            }
            Resolution::Ambiguous(_) => ResolvedLink {
                target: link.target.clone(),
                alias: link.alias.clone(),
                state: "ambiguous".into(),
                id: None,
                kb,
                title: None,
                source_relative: None,
                is_note: false,
            },
            Resolution::None => ResolvedLink {
                target: link.target.clone(),
                alias: link.alias.clone(),
                state: "dangling".into(),
                id: None,
                kb,
                title: None,
                source_relative: None,
                is_note: false,
            },
        };
        out.push(resolved);
    }
    out
}

/// Inbound references to `id` — every artifact that links here, projected to
/// [`BacklinkRef`] (notes first, then alphabetical). One batched `id IN (…)`
/// lookup for all distinct linkers instead of a serial `get_by_id` per
/// linker (the backlink set on a personal kb is small, but a hub note is
/// widely referenced).
pub(crate) async fn load_backlinks(
    ctx: &KbContext,
    kb_name: &KbName,
    id: &str,
) -> Vec<BacklinkRef> {
    let edges = match ctx.storage.backlinks_of(id.to_string()).await {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(kb = %kb_name, id, error = %e, "backlinks_of failed");
            return Vec::new();
        }
    };
    // Distinct linker ids (encounter order — the final sort below is what
    // orders the response, so intermediate order is irrelevant).
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut linker_ids: Vec<String> = Vec::new();
    for edge in edges {
        if seen.insert(edge.from_id.clone()) {
            linker_ids.push(edge.from_id);
        }
    }
    if linker_ids.is_empty() {
        return Vec::new();
    }
    // One `id IN (…)` query resolves every linker in a single actor
    // round-trip; missing/invalid ids are simply absent (same as the old
    // per-id `if let Ok(Some(d))` skip).
    let docs = match ctx.storage.get_by_ids(linker_ids).await {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(kb = %kb_name, id, error = %e, "get_by_ids for backlinks failed");
            return Vec::new();
        }
    };
    let mut out: Vec<BacklinkRef> = docs
        .into_iter()
        .map(|d| BacklinkRef {
            source_relative: rel_of(&d, &ctx.source_path),
            folder: doc_folder(&d.path, &ctx.source_path),
            is_note: kb_core::notes::is_note(&d.path, d.kb_category.as_deref()),
            title: d.title.clone(),
            kb: kb_name.as_str().to_string(),
            id: d.id,
        })
        .collect();
    out.sort_by(|a, b| {
        b.is_note
            .cmp(&a.is_note)
            .then_with(|| a.title.cmp(&b.title))
    });
    out
}

/// `GET /api/kb/{kb}/backlinks/{id}` — inbound references to any artifact
/// ("Referenced in notes/artifacts"). Empty (not 404) when nothing links here.
pub async fn backlinks(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id)): Path<(String, String)>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    Json(BacklinksResponse {
        backlinks: load_backlinks(ctx, &kb_name, &id).await,
    })
    .into_response()
}

/// `GET /api/kb/{kb}/wikilinks/suggest?q=&limit=` — title/basename matches for
/// the composer's `[[` autocomplete. Prefix matches rank above substring
/// matches; ties broken alphabetically. Notes are not privileged (you link
/// research write-ups too) but a note vs artifact is flagged for the icon.
pub async fn suggest(
    State(state): State<Arc<KbHandles>>,
    Path(kb): Path<String>,
    Query(q): Query<SuggestQuery>,
) -> Response<Body> {
    let (_kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let needle = q.q.trim().to_lowercase();
    let limit = q
        .limit
        .unwrap_or(SUGGEST_DEFAULT_LIMIT)
        .min(SUGGEST_MAX_LIMIT);
    // The precomputed, generation-memoised candidate index — one shared
    // corpus scan + lowercased keys instead of a per-keystroke rescan.
    let index = match links_index(ctx).await {
        Ok(i) => i,
        Err(e) => return error_to_problem_json(&e),
    };
    // (rank, lowercased-title, candidate) — rank 0 = title prefix, 1 = title
    // substring, 2 = basename substring; lower wins.
    let mut scored: Vec<(u8, &str, &LinkCandidate)> = Vec::new();
    for e in index.entries.iter() {
        // rank 0 = empty query (list everything) or a title prefix; 1 = title
        // substring; 2 = basename substring; skip otherwise.
        let rank = if needle.is_empty() || e.title_lower.starts_with(&needle) {
            0
        } else if e.title_lower.contains(&needle) {
            1
        } else if e.basename_lower.contains(&needle) {
            2
        } else {
            continue;
        };
        scored.push((rank, e.title_lower.as_str(), e));
    }
    scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));
    scored.truncate(limit);
    Json(SuggestResponse {
        suggestions: scored
            .into_iter()
            .map(|(_, _, e)| WikilinkSuggestion {
                is_note: e.is_note,
                id: e.id.clone(),
                title: e.title.clone(),
                source_relative: e.source_relative.clone(),
            })
            .collect(),
    })
    .into_response()
}

// === CT-F3 — unlinked mentions ==============================================

/// Default queue size for `GET …/links/suggest`; `?limit=` clamps to
/// [`UNLINKED_MAX_LIMIT`]. Mirrors the memory dupes/triage bounds — a
/// triage queue nobody can finish reading is not a queue.
const UNLINKED_DEFAULT_LIMIT: usize = 50;
const UNLINKED_MAX_LIMIT: usize = 200;

/// How many doc bodies one `get_bodies_by_ids` round-trip asks for. The
/// lance filter is a literal `id IN (…)` string, so a whole-corpus list
/// would build one enormous predicate; chunking keeps each query sane
/// without changing the result (ids that miss are simply absent).
const BODY_FETCH_CHUNK: usize = 500;

#[derive(Debug, Deserialize)]
pub struct UnlinkedParams {
    pub limit: Option<usize>,
}

/// One end of a suggested link, enough for a human to recognise it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MentionRef {
    pub id: String,
    pub title: String,
    pub source_relative: String,
    pub is_note: bool,
}

/// One unlinked mention: `src`'s prose names `dst`, and no `kind="link"`
/// edge `src → dst` exists. `applicable` says whether `POST …/links/apply`
/// could author it; when false, `note` says why in one honest line.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct UnlinkedSuggestion {
    pub src: MentionRef,
    pub dst: MentionRef,
    /// The text as it appears in the source (original case).
    pub matched: String,
    /// `"title"` | `"basename"`.
    pub match_kind: String,
    /// The wikilink target that would be authored — the first of the dst's
    /// names that resolves uniquely back to it.
    pub target: String,
    pub applicable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct UnlinkedResponse {
    pub suggestions: Vec<UnlinkedSuggestion>,
    /// Docs whose bodies were actually scanned (transcripts excluded).
    pub scanned: u32,
    /// The name-length floor (`kb_core::mentions::MIN_MENTION_LEN`) —
    /// surfaced so "my short-titled note never appears" has an answer.
    pub min_length: usize,
    /// The limit actually applied.
    pub limit: usize,
}

/// What [`mention_corpus`] hands back: the engine's doc set, plus the
/// display metadata the wire rows are built from (keyed by artifact id).
type MentionCorpus = (
    Vec<kb_core::mentions::MentionDoc>,
    HashMap<String, MentionRef>,
);

/// The engine's corpus snapshot: every live doc as a mention TARGET, plus
/// the bodies to scan as mention SOURCES.
///
/// `scan_only` restricts body loading to one doc id (the apply path): the
/// engine treats an empty body as "target only", so passing the whole
/// corpus with a single body filled in scans exactly one file while still
/// resolving targets — and detecting ambiguity — against the full corpus.
///
/// `memory-session` transcripts are excluded outright, source AND target:
/// R0 (invariant #11) keeps transcripts out of the default read surfaces,
/// they name every artifact a session touched (so every one of them would
/// be a "mention"), and their bodies are the largest rows in the fleet.
async fn mention_corpus(
    ctx: &KbContext,
    scan_only: Option<&str>,
) -> Result<MentionCorpus, kb_core::Error> {
    let (rows, _edge_counts) = crate::routes::docs::gallery_snapshot(ctx).await?;
    let mut docs: Vec<kb_core::mentions::MentionDoc> = Vec::with_capacity(rows.len());
    let mut meta: HashMap<String, MentionRef> = HashMap::with_capacity(rows.len());
    let mut want_body: Vec<String> = Vec::new();

    for row in rows.iter() {
        let d = &row.doc;
        if d.kb_category.as_deref() == Some("memory-session") {
            continue;
        }
        let source_relative = rel_of(d, &ctx.source_path);
        let is_note = kb_core::notes::is_note(&d.path, d.kb_category.as_deref());
        // Applicability tracks the EDGE HOOK's gate (`indexer::is_markdown`),
        // not the per-kb render extension map: a `.txt` mapped to the
        // Markdown pipeline renders `[[…]]` but never records an edge, so
        // calling it applicable would be a lie (see `mentions::applicability`).
        let is_markdown = kb_core::indexer::is_markdown(std::path::Path::new(&d.path));
        let scan = scan_only.is_none_or(|only| only == d.id);
        let mut body = String::new();
        if scan && is_markdown {
            // The raw `.md` (frontmatter split off — a doc's own frontmatter
            // carries its title and would scan as prose). This is the same
            // source the apply path edits, so a suggestion can never name
            // text the editor won't find.
            match tokio::fs::read_to_string(&d.path).await {
                Ok(src) => body = kb_core::notes::split_note_source(&src).1.to_string(),
                Err(e) => {
                    tracing::warn!(path = %d.path, error = %e, "mention scan: markdown read failed")
                }
            }
        } else if scan {
            want_body.push(d.id.clone());
        }
        meta.insert(
            d.id.clone(),
            MentionRef {
                id: d.id.clone(),
                title: d.title.clone(),
                source_relative: source_relative.clone(),
                is_note,
            },
        );
        docs.push(kb_core::mentions::MentionDoc {
            id: d.id.clone(),
            rel_path: source_relative,
            title: d.title.clone(),
            body,
            source: if is_markdown {
                kb_core::mentions::BodySource::Markdown
            } else {
                kb_core::mentions::BodySource::Text
            },
            applicability: kb_core::mentions::applicability(is_markdown, d.kb_category.as_deref()),
        });
    }

    // HTML bodies come from the lance `body` column — the visible text the
    // parser already extracted at index time. kb never re-parses artifact
    // HTML here (invariant #29's "no second html→text pipeline").
    if !want_body.is_empty() {
        let mut by_id: HashMap<String, String> = HashMap::with_capacity(want_body.len());
        for chunk in want_body.chunks(BODY_FETCH_CHUNK) {
            match ctx.storage.get_bodies_by_ids(chunk.to_vec()).await {
                Ok(pairs) => by_id.extend(pairs),
                Err(e) => {
                    tracing::warn!(error = %e, "mention scan: get_bodies_by_ids failed");
                }
            }
        }
        for d in docs.iter_mut() {
            if let Some(body) = by_id.remove(&d.id) {
                d.body = body;
            }
        }
    }
    Ok((docs, meta))
}

fn suggestion_out(
    m: kb_core::mentions::Mention,
    meta: &HashMap<String, MentionRef>,
) -> Option<UnlinkedSuggestion> {
    let src = meta.get(&m.src_id)?.clone();
    let dst = meta.get(&m.dst_id)?.clone();
    Some(UnlinkedSuggestion {
        src,
        dst,
        matched: m.matched,
        match_kind: m.kind.as_str().to_string(),
        target: m.target,
        applicable: m.applicability.is_applicable(),
        note: m.applicability.note().map(str::to_string),
    })
}

/// `GET /api/kb/{kb}/links/suggest[?limit=N]` — CT-F3: the unlinked-mention
/// queue. Docs whose bodies name another artifact's exact title or unique
/// basename with NO `kind="link"` edge to show for it.
///
/// DERIVED PER REQUEST, NEVER PERSISTED — the same posture as the memory
/// dupes/triage reports and the doc↔code bridge's trust classes (invariant
/// #2): no table, no column, no hook, no auto-linking. A row is a claim
/// re-computed from the corpus every time it is read, and the only way it
/// becomes an edge is a human running apply.
///
/// Cost: one memoised corpus scan (`gallery_snapshot`), one read of every
/// Markdown source, one batched `body` projection for the HTML rows, and
/// one `link_pairs()`. That is an on-demand report's budget — the same
/// deliberate choice `/api/memory/dupes` makes with its O(n²) cosine pass.
pub async fn suggest_unlinked(
    State(state): State<Arc<KbHandles>>,
    Path(kb): Path<String>,
    Query(params): Query<UnlinkedParams>,
) -> Response<Body> {
    let (_kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let limit = params
        .limit
        .unwrap_or(UNLINKED_DEFAULT_LIMIT)
        .min(UNLINKED_MAX_LIMIT);
    let (docs, meta) = match mention_corpus(ctx, None).await {
        Ok(v) => v,
        Err(e) => return error_to_problem_json(&e),
    };
    let edges = ctx.storage.link_pairs().await.unwrap_or_default();
    let scanned = docs.iter().filter(|d| !d.body.is_empty()).count() as u32;
    let mentions = kb_core::mentions::find_mentions(&docs, &edges, limit);
    Json(UnlinkedResponse {
        suggestions: mentions
            .into_iter()
            .filter_map(|m| suggestion_out(m, &meta))
            .collect(),
        scanned,
        min_length: kb_core::mentions::MIN_MENTION_LEN,
        limit,
    })
    .into_response()
}

#[derive(Debug, Deserialize)]
pub struct ApplyBody {
    /// Artifact id of the mentioning doc (the one that gets edited).
    pub src: String,
    /// Artifact id of the mentioned doc (the link destination).
    pub dst: String,
}

#[derive(Debug, Serialize)]
pub struct ApplyResponse {
    pub src: MentionRef,
    pub dst: MentionRef,
    pub matched: String,
    pub target: String,
    /// The wikilink exactly as authored, e.g. `[[Deploy checklist|the checklist]]`.
    pub wikilink: String,
    pub source_relative: String,
}

/// `POST /api/kb/{kb}/links/apply` — author ONE suggested wikilink into a
/// Markdown source. The only mutating half of CT-F3, and only ever reached
/// from an explicit human verb (`kb links apply`): nothing on the ingest
/// path calls it, and the queue itself stays derived.
///
/// The mention is RE-DERIVED here from the current corpus rather than
/// trusted from the caller — a queue row can be minutes stale, and the
/// splice must match what is on disk right now. Refusals are loud and
/// leave the tree untouched:
///
/// - `404` — no unlinked mention of `dst` in `src` (already linked, gone,
///   or below the floor),
/// - `400` — `src` can't carry a wikilink (HTML / memory body): the honest
///   `kb_core::mentions::Applicability` note is the problem `detail`,
/// - `409` — the mention exists in the scanned prose but no raw occurrence
///   verifies as spliceable (inline markup, or the file changed).
pub async fn apply_unlinked(
    State(state): State<Arc<KbHandles>>,
    Path(kb): Path<String>,
    Json(body): Json<ApplyBody>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let doc = match ctx.storage.get_by_id(body.src.clone()).await {
        Ok(Some(d)) => d,
        Ok(None) => {
            return error_to_problem_json(&kb_core::Error::NotFound(format!(
                "artifact {} in kb {kb_name}",
                body.src
            )))
        }
        Err(e) => return error_to_problem_json(&e),
    };
    // Scan ONLY the source doc, but resolve names against the whole corpus.
    let (docs, meta) = match mention_corpus(ctx, Some(&body.src)).await {
        Ok(v) => v,
        Err(e) => return error_to_problem_json(&e),
    };
    let edges = ctx.storage.link_pairs().await.unwrap_or_default();
    let mention = kb_core::mentions::find_mentions(&docs, &edges, usize::MAX)
        .into_iter()
        .find(|m| m.src_id == body.src && m.dst_id == body.dst);
    let Some(mention) = mention else {
        return error_to_problem_json(&kb_core::Error::NotFound(format!(
            "no unlinked mention of {} in {} (already linked, or the text changed)",
            body.dst, body.src
        )));
    };
    if let Some(note) = mention.applicability.note() {
        return error_to_problem_json(&kb_core::Error::BadRequest(format!("{}: {note}", body.src)));
    }

    let src_text = match tokio::fs::read_to_string(&doc.path).await {
        Ok(s) => s,
        Err(e) => {
            return error_to_problem_json(&kb_core::Error::Storage(format!(
                "read {}: {e}",
                doc.path
            )))
        }
    };
    let (_, old_body) = kb_core::notes::split_note_source(&src_text);
    let Some(new_body) =
        kb_core::mentions::apply_wikilink(old_body, &mention.matched, &mention.target)
    else {
        return error_to_problem_json(&kb_core::Error::Conflict(format!(
            "{:?} is no longer spliceable in {} — link it by hand",
            mention.matched, body.src
        )));
    };
    let new_src = kb_core::notes::replace_body(&src_text, &new_body);
    if let Err(e) = kb_core::fsx::write_atomic(std::path::Path::new(&doc.path), new_src.as_bytes())
    {
        return error_to_problem_json(&e);
    }
    // A note write emits the note SSE the SPA listens for; every Markdown
    // artifact gets the indexer nudge, so the new edge lands without waiting
    // on inotify.
    if kb_core::notes::is_note(&doc.path, doc.kb_category.as_deref()) {
        crate::routes::notes::emit_updated(ctx, &kb_name, &body.src, &doc).await;
    } else {
        crate::routes::notes::nudge_indexer(ctx, std::path::Path::new(&doc.path)).await;
    }

    let wikilink = if mention.matched == mention.target {
        format!("[[{}]]", mention.target)
    } else {
        format!("[[{}|{}]]", mention.target, mention.matched)
    };
    let (Some(src), Some(dst)) = (meta.get(&body.src).cloned(), meta.get(&body.dst).cloned())
    else {
        return error_to_problem_json(&kb_core::Error::NotFound(format!(
            "artifact {} or {} in kb {kb_name}",
            body.src, body.dst
        )));
    };
    let source_relative = src.source_relative.clone();
    Json(ApplyResponse {
        src,
        dst,
        matched: mention.matched,
        target: mention.target,
        wikilink,
        source_relative,
    })
    .into_response()
}
