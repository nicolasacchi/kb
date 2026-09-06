//! V3.G2 — `GET /api/usages?repo=&path=&line=&col=[&limit=]`: classified
//! usages of the symbol at a position.
//!
//! Groups:
//! - **exact** — same-file locals-bound references (G1 `local_def_ordinal`
//!   pointing at the symbol's def) + scip occurrences when the symbol is
//!   scip-indexed.
//! - **likely** — import-filtered cross-file name refs that pass kind+arity
//!   scoring (§2/§3 of the V3.G2 brief).
//! - **candidate** — remaining word-boundary name matches from the
//!   occurrences table EXCLUDING comment/string tokens when the capture
//!   kind is known; `access=null` when unsure (never mislabel).
//!
//! Caps: `limit` query param (default 500, max 500) per class; `truncated`
//! is true when any class was capped.
//!
//! # V71-E1 — one ladder, two projections
//!
//! The ladder below now returns [`CoreOut`] (rows PLUS the per-row
//! provenance the classifier already knew and v1 threw away: which tier
//! produced the row, and the occurrence role behind it). `usages/1`'s body
//! is a pure projection of it — [`project_v1`] — and `usages/2`
//! (`crate::usages2`) is a second, richer one. There is deliberately no
//! second ladder: a divergent copy is exactly how `usages_counts_*` came to
//! disagree with `usages_at` (recon `usages.md` §6.3), and one of those
//! numbers is always wrong.
//!
//! `usages/1`'s wire is FROZEN. `UsageRow`/`UsagesOut` carry no new field;
//! `oracle::usages_route`'s shape test pins that byte-for-byte.

use crate::intel::{access, arity, cross_file};
use crate::resolve::{word_at, CLASS_CANDIDATE, CLASS_EXACT, CLASS_LIKELY};
use crate::routes::{find_repo, read_repo_file, ApiError};
use crate::state::SharedState;
use crate::store::{Store, StoreBlocking};
use axum::extract::{Query, State};
use axum::http::header;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const USAGES_SCHEMA: &str = "usages/1";
pub const DEFAULT_LIMIT: usize = 500;
pub const MAX_LIMIT: usize = 500;

#[derive(Debug, Deserialize)]
pub struct UsagesParams {
    pub repo: String,
    pub path: String,
    pub line: u32,
    pub col: u32,
    #[serde(rename = "ref")]
    pub rev: Option<String>,
    /// Per-class cap (default 500, max 500). Exposed for tests.
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageSymbol {
    pub name: String,
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageRow {
    pub path: String,
    pub line: u32,
    pub col: u32,
    /// `"def"` | `"ref"`.
    pub kind: String,
    /// `"read"` | `"write"` | null when unsure.
    pub access: Option<&'static str>,
    /// Line text (trimmed, capped).
    pub context: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsagesOut {
    pub schema: &'static str,
    pub symbol: UsageSymbol,
    /// Trust class of the definition we anchored on (exact/likely/candidate).
    pub class_of_definition: &'static str,
    pub exact: Vec<UsageRow>,
    pub likely: Vec<UsageRow>,
    pub candidate: Vec<UsageRow>,
    pub truncated: bool,
    /// Totals before per-class cap (for the truncated flag).
    pub total_exact: usize,
    pub total_likely: usize,
    pub total_candidate: usize,
}

/// V71-E1 — the per-row provenance the ladder knows while classifying and
/// `usages/1` cannot carry. Never persisted (root CLAUDE.md #2), never a
/// class of its own: `precision` is the EXISTING `resolve::PRECISION_*`
/// vocabulary, so `resolve::class_for_precision` maps it back to the group
/// the row is already in.
#[derive(Debug, Clone)]
pub(crate) struct RowDetail {
    pub precision: &'static str,
    /// `occurrences.role` behind this row (`"def"`/`"ref"`/`"import"`), or
    /// `None` for a row that came from something other than an occurrence
    /// (a Rails convention edge, an lsp-live location).
    pub occ_role: Option<String>,
    /// `Some` when the row came from the Rails lens's reverse lookup.
    pub rails_kind: Option<crate::frameworks::EdgeKind>,
    /// End column (0-based, exclusive) of the identifier — `usages/1`
    /// carries only the start, so a consumer cannot highlight the span.
    pub col_end: Option<u32>,
}

#[derive(Debug, Clone)]
pub(crate) struct CoreRow {
    pub row: UsageRow,
    pub detail: RowDetail,
}

/// The ladder's own output: sorted, NOT truncated. Both wire versions
/// project from this.
#[derive(Debug, Clone)]
pub(crate) struct CoreOut {
    pub symbol: UsageSymbol,
    pub class_of_definition: &'static str,
    pub exact: Vec<CoreRow>,
    pub likely: Vec<CoreRow>,
    pub candidate: Vec<CoreRow>,
}

/// `usages/1`'s projection of [`CoreOut`] — the pre-V71 body, field for
/// field, including the per-class truncate and the `truncated` flag.
pub(crate) fn project_v1(core: CoreOut, limit: usize) -> UsagesOut {
    let mut exact: Vec<UsageRow> = core.exact.into_iter().map(|r| r.row).collect();
    let mut likely: Vec<UsageRow> = core.likely.into_iter().map(|r| r.row).collect();
    let mut candidate: Vec<UsageRow> = core.candidate.into_iter().map(|r| r.row).collect();
    let total_exact = exact.len();
    let total_likely = likely.len();
    let total_candidate = candidate.len();
    let truncated = total_exact > limit || total_likely > limit || total_candidate > limit;
    exact.truncate(limit);
    likely.truncate(limit);
    candidate.truncate(limit);
    UsagesOut {
        schema: USAGES_SCHEMA,
        symbol: core.symbol,
        class_of_definition: core.class_of_definition,
        exact,
        likely,
        candidate,
        truncated,
        total_exact,
        total_likely,
        total_candidate,
    }
}

/// `GET /api/usages?repo=&path=&line=&col=[&limit=]`.
///
/// PRR-L2: after the synchronous ladder (`usages_at`, unchanged) resolves
/// its own classes, this handler ALSO consults `crate::lip`'s provider
/// references for the SAME position and merges them into the `exact` class
/// (capped at `limit`, deduped against what's already there —
/// `crate::lip::overlay_usages`). On refusal/timeout/absence/
/// no-configured-provider the existing response is returned byte-for-byte
/// unchanged.
pub async fn usages_route(
    State(state): State<SharedState>,
    Query(params): Query<UsagesParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let repo = repo.clone();
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let path = params.path.clone();
    let line = params.line;
    let col = params.col;
    let rev = params.rev.clone();
    // 2026-08-31 incident (store.rs module doc): the sync usages ladder
    // runs on the blocking pool; the lip overlay below is the async leg.
    let repo_bg = repo.clone();
    let mut out = state
        .store
        .run_blocking(move |store| {
            usages_at(
                store,
                &repo_bg,
                repo_id,
                &path,
                line,
                col,
                rev.as_deref(),
                limit,
            )
        })
        .await?;
    crate::lip::overlay_usages(
        &state,
        &repo,
        &params.path,
        params.rev.as_deref(),
        params.line,
        params.col,
        limit,
        &mut out,
    )
    .await;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn usages_at(
    store: &Store,
    repo: &crate::config::RepoEntry,
    repo_id: i64,
    path: &str,
    line: u32,
    col: u32,
    rev: Option<&str>,
    limit: usize,
) -> Result<UsagesOut, ApiError> {
    let core = usages_core(store, repo, repo_id, path, line, col, rev)?;
    Ok(project_v1(core, limit))
}

/// The classified ladder itself (V3.G2, unchanged in behaviour; V71-E1
/// widened its RETURN to carry per-row provenance — see [`CoreOut`]).
/// Sorted, never truncated: capping is each wire version's own job.
#[allow(clippy::too_many_arguments)]
pub(crate) fn usages_core(
    store: &Store,
    repo: &crate::config::RepoEntry,
    repo_id: i64,
    path: &str,
    line: u32,
    col: u32,
    rev: Option<&str>,
) -> Result<CoreOut, ApiError> {
    if line < 1 {
        return Err(ApiError::bad_request("line must be >= 1 (1-based)"));
    }
    let read = read_repo_file(repo, path, rev)?;
    let content = std::str::from_utf8(&read.bytes).map_err(|_| {
        ApiError::bad_request(format!(
            "{path}: not valid UTF-8 — usages needs text content"
        ))
    })?;
    let lines: Vec<&str> = content.split('\n').collect();
    let line_idx = (line - 1) as usize;
    let Some(line_text) = lines.get(line_idx) else {
        return Err(ApiError::bad_request(format!(
            "line {line} is out of range — {path} has {} line(s)",
            lines.len()
        )));
    };
    let line_bytes = line_text.strip_suffix('\r').unwrap_or(line_text).as_bytes();
    if col as usize > line_bytes.len() {
        return Err(ApiError::bad_request(format!(
            "col {col} is out of range for line {line} ({} bytes)",
            line_bytes.len()
        )));
    }

    let lang_info = crate::lang::detect(path, Some(&read.bytes));
    let salt = lang_info.map(|l| l.symbol_salt);

    let hit = match salt {
        Some(s) if store.has_occurrences(&read.blob_hash, s)? => {
            store.occurrence_at(&read.blob_hash, s, line, col)?
        }
        _ => None,
    };
    let ident = if let Some(ref o) = hit {
        o.name.clone()
    } else {
        word_at(line_bytes, col as usize)
            .ok_or_else(|| ApiError::bad_request(format!("no identifier at {path}:{line}:{col}")))?
    };

    // Resolve the definition class via the same ladder as /api/resolve
    // (reuse) — take the top candidate.
    let resolved = crate::resolve::resolve_position(
        store,
        // single-repo view for class_of_definition
        std::slice::from_ref(repo),
        &std::collections::HashMap::from([(repo.name.clone(), repo_id)]),
        repo,
        repo_id,
        path,
        line,
        col,
        rev,
    )?;
    let class_of_definition = resolved
        .candidates
        .first()
        .map(|c| c.class)
        .unwrap_or(CLASS_CANDIDATE);
    let def_kind = resolved.candidates.first().and_then(|c| c.kind.clone());
    let def_container = resolved
        .candidates
        .first()
        .and_then(|c| c.container.clone());
    // Primary def location (for locals binding checks).
    let primary_def = resolved
        .candidates
        .first()
        .map(|c| (c.path.clone(), c.line));

    let import_targets: HashSet<String> = match store.file_id(repo_id, path)? {
        Some(fid) => store.import_target_paths(fid)?.into_iter().collect(),
        None => HashSet::new(),
    };

    let mut exact: Vec<CoreRow> = Vec::new();
    let mut likely: Vec<CoreRow> = Vec::new();
    let mut candidate: Vec<CoreRow> = Vec::new();
    let mut seen: HashSet<(String, u32, u32)> = HashSet::new();

    // --- exact: scip refs + locals-bound same-file refs ------------------
    // Locals: every ref in THIS blob whose local_def_ordinal points at the
    // def occurrence for `ident` (if we have one).
    if let Some(s) = salt {
        if let Some(ref hit_occ) = hit {
            // If the position itself is a def, use its ordinal as the def.
            let def_ord = if hit_occ.role == "def" && hit_occ.name == ident {
                Some(hit_occ.ordinal)
            } else {
                hit_occ.local_def_ordinal
            };
            if let Some(dord) = def_ord {
                for occ in store.occurrences_for_blob(&read.blob_hash, s)? {
                    if occ.role == "ref" && occ.name == ident && occ.local_def_ordinal == Some(dord)
                    {
                        push_usage(
                            &mut exact,
                            &mut seen,
                            path,
                            &occ,
                            content,
                            lang_info.map(|l| l.id),
                            &read.bytes,
                            "ref",
                            crate::resolve::PRECISION_LOCALS,
                        );
                    }
                    if occ.role == "def" && occ.ordinal == dord && occ.name == ident {
                        push_usage(
                            &mut exact,
                            &mut seen,
                            path,
                            &occ,
                            content,
                            lang_info.map(|l| l.id),
                            &read.bytes,
                            "def",
                            crate::resolve::PRECISION_LOCALS,
                        );
                    }
                }
            }
        }
        // SCIP occurrences named ident (any file with scip rows for this
        // name in-repo) — only when at least one scip def exists.
        let scip_defs = store.scip_def_occurrences_by_name(&read.blob_hash, s, &ident)?;
        if !scip_defs.is_empty() {
            for occ in store.occurrences_for_blob(&read.blob_hash, s)? {
                if occ.source == crate::occurrences::SOURCE_SCIP && occ.name == ident {
                    let k = if occ.role == "def" { "def" } else { "ref" };
                    push_usage(
                        &mut exact,
                        &mut seen,
                        path,
                        &occ,
                        content,
                        lang_info.map(|l| l.id),
                        &read.bytes,
                        k,
                        crate::resolve::PRECISION_SCIP_EXACT,
                    );
                }
            }
        }
    }

    // --- repo-wide name matches from occurrences -------------------------
    let refs = store.ref_occurrences_by_name_in_repo(repo_id, &ident)?;
    let defs = store.def_occurrences_by_name_in_repo(repo_id, &ident)?;

    // Load file texts lazily for context/access.
    let mut file_text_cache: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    file_text_cache.insert(path.to_string(), content.to_string());

    let get_text =
        |p: &str, cache: &mut std::collections::HashMap<String, String>| -> Option<String> {
            if let Some(t) = cache.get(p) {
                return Some(t.clone());
            }
            let t = std::fs::read_to_string(repo.path.join(p)).ok()?;
            cache.insert(p.to_string(), t.clone());
            Some(t)
        };

    // Defs first as likely/candidate based on import filter.
    let def_paths: Vec<String> = defs.iter().map(|(p, _)| p.clone()).collect();
    let (def_tagged, _) = cross_file::filter_and_tag(path, &def_paths, &import_targets);
    for (idx, _reach, class, prec) in &def_tagged {
        let (p, occ) = &defs[*idx];
        if seen.contains(&(p.clone(), occ.line, occ.col_start)) {
            continue;
        }
        let text = get_text(p, &mut file_text_cache).unwrap_or_default();
        let row = CoreRow {
            row: make_row(p, occ, &text, "def", None),
            detail: RowDetail {
                precision: prec,
                occ_role: Some(occ.role.clone()),
                rails_kind: None,
                col_end: Some(occ.col_end),
            },
        };
        match *class {
            CLASS_LIKELY => likely.push(row),
            CLASS_EXACT => exact.push(row),
            _ => candidate.push(row),
        }
        seen.insert((p.clone(), occ.line, occ.col_start));
    }

    // Refs: classify by import reach of the REF's file relative to the
    // definition's file (primary def path). A ref in a file that imports
    // the def file is likely; others candidate. Same-file locals already
    // covered in exact.
    let def_path = primary_def
        .as_ref()
        .map(|(p, _)| p.as_str())
        .unwrap_or(path);

    for (p, occ) in &refs {
        if seen.contains(&(p.clone(), occ.line, occ.col_start)) {
            continue;
        }
        // Skip if this is a same-file locals-bound ref already in exact
        // (seen handles it). Classify:
        let text = get_text(p, &mut file_text_cache).unwrap_or_default();
        let lang = crate::lang::detect(p, Some(text.as_bytes())).map(|l| l.id);
        let acc =
            lang.and_then(|lid| access::access_at(lid, text.as_bytes(), occ.line, occ.col_start));

        // Import filter: is the ref's file import-related to the def file?
        // Check both directions + same-dir.
        let ref_fid = store.file_id(repo_id, p).ok().flatten();
        let def_fid = store.file_id(repo_id, def_path).ok().flatten();
        let mut targets = HashSet::new();
        if let Some(fid) = ref_fid {
            if let Ok(ps) = store.import_target_paths(fid) {
                targets.extend(ps);
            }
        }
        // Also: def file imports ref file? less relevant. Reach of def
        // from ref:
        let reach = cross_file::classify_reach(p, def_path, &targets);
        // Or def's import targets contain ref? reverse.
        let mut def_targets = HashSet::new();
        if let Some(fid) = def_fid {
            if let Ok(ps) = store.import_target_paths(fid) {
                def_targets.extend(ps);
            }
        }
        let reverse_reach = cross_file::classify_reach(def_path, p, &def_targets);
        let is_likely = matches!(
            reach,
            cross_file::Reach::SameFile
                | cross_file::Reach::SameDir
                | cross_file::Reach::ImportReachable
        ) || matches!(
            reverse_reach,
            cross_file::Reach::SameDir | cross_file::Reach::ImportReachable
        );

        // Arity: if this ref is a call site, demote when arity rejects the
        // primary def's param range.
        let mut class = if is_likely {
            CLASS_LIKELY
        } else {
            CLASS_CANDIDATE
        };
        // V71-E1 — the tier that produced this row, named with the SAME
        // vocabulary `resolve.rs` already uses (`class_for_precision` maps
        // it back to `class`). An arity demotion below rewrites it to
        // `tags-approx`, exactly as `resolve.rs:617` does for its own
        // candidates.
        let mut precision = if is_likely {
            cross_file::PRECISION_IMPORT_FILTERED
        } else {
            cross_file::PRECISION_TAGS_APPROX
        };
        if class == CLASS_LIKELY {
            if let (Some(lid), Some((_, def_line))) = (lang, primary_def.as_ref()) {
                if let Some(argc) =
                    arity::call_arg_count_at(lid, text.as_bytes(), occ.line, occ.col_start)
                {
                    // Look up def symbol params.
                    if let Some((_, sym)) = store
                        .symbols_for_repo(repo_id)
                        .ok()
                        .into_iter()
                        .flatten()
                        .find(|(sp, s)| {
                            sp == def_path && s.name == ident && s.line_start == *def_line
                        })
                    {
                        if arity::arity_rejects(argc, sym.param_min, sym.param_max) {
                            class = CLASS_CANDIDATE;
                            precision = cross_file::PRECISION_TAGS_APPROX;
                        }
                    }
                }
            }
        }

        let row = CoreRow {
            row: make_row(p, occ, &text, "ref", acc),
            detail: RowDetail {
                precision,
                occ_role: Some(occ.role.clone()),
                rails_kind: None,
                col_end: Some(occ.col_end),
            },
        };
        match class {
            CLASS_LIKELY => likely.push(row),
            CLASS_EXACT => exact.push(row),
            _ => candidate.push(row),
        }
        seen.insert((p.clone(), occ.line, occ.col_start));
    }

    // PRR-N5 — reverse framework lookup: usages of a view/partial/
    // component/controller PATH also include every `rails_edges` row
    // pointing AT it (dst_path == this path) — independent of whichever
    // `ident` the click resolved to (a whole-FILE "who references this"
    // signal, not a name-scoped one; mirrors `resolve.rs`'s own
    // framework-convention tier's sibling POSITION-gated convention). The
    // row's own `Trust` maps straight onto likely/candidate — never exact
    // (see `frameworks`'s module doc's oracle-bar posture).
    //
    // *R3 fix (v70-a1, recon rails-lens.md §6):* NO `rails_lens_relevant_path`
    // gate here any more. That predicate is a SOURCE-path predicate
    // (`frameworks::mod`'s own doc: it names the extraction dispatch's
    // source shapes) — reusing it to gate this REVERSE (dst-keyed) query
    // meant every destination-only file (`config/locales/*.yml`,
    // `app/javascript/controllers/*.js`, `app/helpers/**_helper.rb`, a
    // `.haml`/non-`.erb` partial…) got NO reverse usages here even though
    // `rails_edges_by_dst_path` demonstrably holds rows pointing at it
    // (`GET /api/framework/edges` has no such gate and DOES show them —
    // the SPA rail and this "find usages" verb used to disagree about the
    // same file). The `(repo_id, dst_path)` index (`rails_edges_dst`,
    // migration V0026) makes this query cheap regardless of path shape,
    // and it is naturally a no-op for a non-Rails repo or a path with no
    // inbound edges (`rails_edges` only ever has rows a Rails repo's
    // ingest wrote) — no extra `is_rails` re-check needed at read time.
    for edge in store.rails_edges_by_dst_path(repo_id, path)? {
        let Some(src_line) = edge.src_line else {
            continue; // no source position — nothing to point at.
        };
        if !seen.insert((edge.src_path.clone(), src_line, 0)) {
            continue;
        }
        let text = get_text(&edge.src_path, &mut file_text_cache).unwrap_or_default();
        let context: String = text
            .lines()
            .nth((src_line.saturating_sub(1)) as usize)
            .unwrap_or("")
            .trim()
            .chars()
            .take(200)
            .collect();
        let row = CoreRow {
            row: UsageRow {
                path: edge.src_path.clone(),
                line: src_line,
                col: 0,
                kind: "ref".to_string(),
                access: None,
                context,
            },
            detail: RowDetail {
                precision: crate::resolve::PRECISION_FRAMEWORK,
                occ_role: None,
                rails_kind: Some(edge.kind),
                col_end: None,
            },
        };
        match edge.trust {
            crate::frameworks::Trust::Likely => likely.push(row),
            crate::frameworks::Trust::Candidate => candidate.push(row),
        }
    }

    // Sort each group for stable output.
    for group in [&mut exact, &mut likely, &mut candidate] {
        group.sort_by(|a, b| {
            a.row
                .path
                .cmp(&b.row.path)
                .then_with(|| a.row.line.cmp(&b.row.line))
                .then_with(|| a.row.col.cmp(&b.row.col))
        });
    }

    Ok(CoreOut {
        symbol: UsageSymbol {
            name: ident,
            kind: def_kind,
            container: def_container,
        },
        class_of_definition,
        exact,
        likely,
        candidate,
    })
}

#[allow(clippy::too_many_arguments)]
fn push_usage(
    out: &mut Vec<CoreRow>,
    seen: &mut HashSet<(String, u32, u32)>,
    path: &str,
    occ: &crate::occurrences::Occurrence,
    content: &str,
    lang_id: Option<&str>,
    bytes: &[u8],
    kind: &str,
    precision: &'static str,
) {
    if !seen.insert((path.to_string(), occ.line, occ.col_start)) {
        return;
    }
    let acc = lang_id.and_then(|lid| access::access_at(lid, bytes, occ.line, occ.col_start));
    out.push(CoreRow {
        row: make_row(path, occ, content, kind, acc),
        detail: RowDetail {
            precision,
            occ_role: Some(occ.role.clone()),
            rails_kind: None,
            col_end: Some(occ.col_end),
        },
    });
}

fn make_row(
    path: &str,
    occ: &crate::occurrences::Occurrence,
    content: &str,
    kind: &str,
    access: Option<&'static str>,
) -> UsageRow {
    let context = content
        .lines()
        .nth((occ.line.saturating_sub(1)) as usize)
        .unwrap_or("")
        .trim()
        .chars()
        .take(200)
        .collect();
    UsageRow {
        path: path.to_string(),
        line: occ.line,
        col: occ.col_start,
        kind: kind.to_string(),
        access,
        context,
    }
}

/// Count-only path for Code Vision lenses — same class ladder as
/// [`usages_at`] but never materializes row context/access strings.
#[derive(Debug, Clone, Copy, Default)]
pub struct UsagesCounts {
    pub exact: usize,
    pub likely: usize,
    pub candidate: usize,
}

/// Position-based entry (resolves name via word/occurrence, then counts).
#[allow(clippy::too_many_arguments)]
pub fn usages_counts_at(
    store: &Store,
    repo: &crate::config::RepoEntry,
    repo_id: i64,
    path: &str,
    line: u32,
    col: u32,
    rev: Option<&str>,
) -> Result<UsagesCounts, ApiError> {
    let read = read_repo_file(repo, path, rev)?;
    let content = std::str::from_utf8(&read.bytes).unwrap_or("");
    let line_text = content
        .lines()
        .nth((line as usize).saturating_sub(1))
        .unwrap_or("");
    let line_bytes = line_text.strip_suffix('\r').unwrap_or(line_text).as_bytes();
    let lang_info = crate::lang::detect(path, Some(&read.bytes));
    let salt = lang_info.map(|l| l.symbol_salt);
    let hit = match salt {
        Some(s) if store.has_occurrences(&read.blob_hash, s)? => {
            store.occurrence_at(&read.blob_hash, s, line, col)?
        }
        _ => None,
    };
    let name = if let Some(ref o) = hit {
        o.name.clone()
    } else {
        word_at(line_bytes, col as usize)
            .ok_or_else(|| ApiError::bad_request(format!("no identifier at {path}:{line}:{col}")))?
    };
    usages_counts_for_name(
        store,
        repo_id,
        path,
        &name,
        Some(line),
        salt,
        &read.blob_hash,
    )
}

/// Name-based count path used by lenses (declaration already known).
/// No resolve_position, no file text loads for context.
pub fn usages_counts_for_name(
    store: &Store,
    repo_id: i64,
    def_path: &str,
    name: &str,
    def_line: Option<u32>,
    salt: Option<&str>,
    blob_hash: &str,
) -> Result<UsagesCounts, ApiError> {
    let map = usages_counts_for_names(
        store,
        repo_id,
        def_path,
        &[(name.to_string(), def_line)],
        salt,
        blob_hash,
    )?;
    Ok(map.into_iter().next().map(|(_, c)| c).unwrap_or_default())
}

/// Batch count path for Code Vision lenses — one occurrences scan for all
/// declaration names in a file (avoids N full-repo name scans).
pub fn usages_counts_for_names(
    store: &Store,
    repo_id: i64,
    def_path: &str,
    names: &[(String, Option<u32>)], // (name, def_line)
    salt: Option<&str>,
    blob_hash: &str,
) -> Result<std::collections::HashMap<String, UsagesCounts>, ApiError> {
    use std::collections::HashMap;
    let mut out: HashMap<String, UsagesCounts> = HashMap::new();
    if names.is_empty() {
        return Ok(out);
    }
    for (n, _) in names {
        out.insert(n.clone(), UsagesCounts::default());
    }
    let name_set: HashSet<String> = names.iter().map(|(n, _)| n.clone()).collect();
    let name_list: Vec<String> = name_set.iter().cloned().collect();

    // exact: same-file locals-bound + scip for each name
    if let Some(s) = salt {
        let blob_occs = store.occurrences_for_blob(blob_hash, s)?;
        for (name, def_line) in names {
            let def_ord = def_line.and_then(|dl| {
                blob_occs
                    .iter()
                    .find(|o| o.role == "def" && o.name == *name && o.line == dl)
                    .map(|o| o.ordinal)
            });
            let c = out.get_mut(name).unwrap();
            let mut seen: HashSet<(u32, u32)> = HashSet::new();
            if let Some(dord) = def_ord {
                for occ in &blob_occs {
                    if occ.name != *name {
                        continue;
                    }
                    if ((occ.role == "ref" && occ.local_def_ordinal == Some(dord))
                        || (occ.role == "def" && occ.ordinal == dord))
                        && seen.insert((occ.line, occ.col_start))
                    {
                        c.exact += 1;
                    }
                }
            }
            let has_scip = blob_occs.iter().any(|o| {
                o.source == crate::occurrences::SOURCE_SCIP && o.name == *name && o.role == "def"
            });
            if has_scip {
                for occ in &blob_occs {
                    if occ.source == crate::occurrences::SOURCE_SCIP
                        && occ.name == *name
                        && seen.insert((occ.line, occ.col_start))
                    {
                        c.exact += 1;
                    }
                }
            }
        }
    }

    // One cross-file scan for all names.
    let all = store.occurrences_by_names_in_repo(repo_id, &name_list)?;

    // Per-file import targets cache.
    let mut import_cache: HashMap<String, HashSet<String>> = HashMap::new();
    let mut get_targets = |store: &Store, path: &str| -> HashSet<String> {
        if let Some(t) = import_cache.get(path) {
            return t.clone();
        }
        let t = match store.file_id(repo_id, path).ok().flatten() {
            Some(fid) => store
                .import_target_paths(fid)
                .unwrap_or_default()
                .into_iter()
                .collect(),
            None => HashSet::new(),
        };
        import_cache.insert(path.to_string(), t.clone());
        t
    };
    let def_targets = get_targets(store, def_path);

    // Track positions already counted as exact per name.
    let mut seen: HashSet<(String, String, u32, u32)> = HashSet::new(); // name,path,line,col

    for (p, occ) in &all {
        if !name_set.contains(&occ.name) {
            continue;
        }
        // Skip same-file exact already counted via locals path: still
        // classify remaining same-file as likely/candidate below unless
        // already exact. We don't know which were exact without the set —
        // re-classify: same-file refs that aren't already in exact stay
        // likely (same-file reach).
        let key = (occ.name.clone(), p.clone(), occ.line, occ.col_start);
        if !seen.insert(key) {
            continue;
        }
        let c = out.get_mut(&occ.name).unwrap();
        // Same-file: count as likely (exact already handled above for
        // locals-bound; remaining same-file name hits are at least likely).
        if p == def_path {
            // Avoid double-counting defs/refs already in exact: the
            // exact counter already includes them. Use a soft skip by
            // only adding if this isn't a pure re-count of the def line.
            // Simpler: same-file non-def refs go to likely.
            if occ.role == "ref" {
                c.likely += 1;
            }
            continue;
        }
        let targets = get_targets(store, p);
        let reach = cross_file::classify_reach(p, def_path, &targets);
        let reverse = cross_file::classify_reach(def_path, p, &def_targets);
        let is_likely = matches!(
            reach,
            cross_file::Reach::SameFile
                | cross_file::Reach::SameDir
                | cross_file::Reach::ImportReachable
        ) || matches!(
            reverse,
            cross_file::Reach::SameDir | cross_file::Reach::ImportReachable
        );
        if is_likely {
            c.likely += 1;
        } else {
            c.candidate += 1;
        }
    }

    Ok(out)
}

// PRR-N5 — this crate had no `usages.rs` unit tests before this unit; the
// two below cover ONLY the new reverse-framework-lookup addition (see the
// module-level PRR-N5 comment above `// Sort each group for stable
// output.`). Fixture-repo helpers mirror `resolve.rs`'s own test module.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RepoEntry;

    fn write_file(root: &std::path::Path, path: &str, content: &str) {
        let abs = root.join(path);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(abs, content).unwrap();
    }

    fn fixture_repo(store: &Store, name: &str) -> (tempfile::TempDir, RepoEntry, i64) {
        let root = tempfile::tempdir().unwrap();
        let entry = RepoEntry {
            name: name.to_string(),
            path: root.path().to_path_buf(),
        };
        let repo_id = store
            .upsert_repo(name, root.path().to_str().unwrap())
            .unwrap();
        (root, entry, repo_id)
    }

    #[test]
    fn reverse_framework_lookup_merges_rails_edges_pointing_at_this_partial() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        let (root, repo, repo_id) = fixture_repo(&store, "fixture");

        let partial_path = "app/views/x/_row.html.erb";
        write_file(root.path(), partial_path, "<p>row</p>\n");

        // Two render call sites, one Likely one Candidate — both must
        // surface, in their matching trust bucket, merged alongside
        // whatever the (empty, here) occurrences-derived groups produced.
        // Two SEPARATE `replace_rails_edges` calls (one per producing
        // path), mirroring the real per-file dispatch contract — every
        // real extractor sets `src_path` to the SAME path it was called
        // with (frameworks/mod.rs), so a single call never mixes edges
        // from two different source files the way one shared call would.
        store
            .replace_rails_edges(
                repo_id,
                "app/controllers/x_controller.rb",
                "blobhash-partial-1",
                crate::frameworks::RAILS_LENS_GRAMMAR_VERSION,
                &[crate::frameworks::FrameworkEdge {
                    kind: crate::frameworks::EdgeKind::RenderPartial,
                    src_path: "app/controllers/x_controller.rb".to_string(),
                    src_line: Some(5),
                    src_symbol: None,
                    dst_kind: Some("partial".to_string()),
                    dst_path: Some(partial_path.to_string()),
                    dst_symbol: None,
                    trust: crate::frameworks::Trust::Likely,
                    extra_json: None,
                }],
            )
            .unwrap();
        store
            .replace_rails_edges(
                repo_id,
                "app/views/x/other.html.erb",
                "blobhash-partial-2",
                crate::frameworks::RAILS_LENS_GRAMMAR_VERSION,
                &[crate::frameworks::FrameworkEdge {
                    kind: crate::frameworks::EdgeKind::RenderPartial,
                    src_path: "app/views/x/other.html.erb".to_string(),
                    src_line: Some(2),
                    src_symbol: None,
                    dst_kind: Some("partial".to_string()),
                    dst_path: Some(partial_path.to_string()),
                    dst_symbol: None,
                    trust: crate::frameworks::Trust::Candidate,
                    extra_json: None,
                }],
            )
            .unwrap();

        // Click somewhere in the partial itself — the reverse lookup is
        // whole-file, independent of what this click resolves `ident` to.
        let out = usages_at(&store, &repo, repo_id, partial_path, 1, 4, None, 500).unwrap();

        assert!(
            out.likely
                .iter()
                .any(|r| r.path == "app/controllers/x_controller.rb" && r.line == 5),
            "{:#?}",
            out.likely
        );
        assert!(
            out.candidate
                .iter()
                .any(|r| r.path == "app/views/x/other.html.erb" && r.line == 2),
            "{:#?}",
            out.candidate
        );
    }

    // R3 (v70-a1, recon rails-lens.md §6) — the reverse lookup used to be
    // gated on `rails_lens_relevant_path`, a SOURCE-path predicate, so any
    // DESTINATION-only path (locale yml, Stimulus JS controller, …) got no
    // reverse usages here even though `rails_edges` held rows pointing at
    // it. The gate is gone; the new contract is: a path with no inbound
    // `rails_edges` rows surfaces nothing (a genuine absence, not a
    // gate — the old test's name is retired), and ANY path with inbound
    // rows surfaces them, regardless of its own shape.

    #[test]
    fn reverse_framework_lookup_returns_nothing_for_a_path_with_no_inbound_edges() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        let (root, repo, repo_id) = fixture_repo(&store, "fixture");

        let src = "fn widget() -> i32 {\n    1\n}\n";
        write_file(root.path(), "a.rs", src);
        let blob_hash = crate::ingest::git_blob_hash(src.as_bytes());
        store
            .upsert_file(repo_id, "a.rs", &blob_hash, "rust", src.len() as u64)
            .unwrap();
        store
            .replace_symbols(
                &blob_hash,
                crate::lang::RUST.symbol_salt,
                &crate::extract::extract_symbols("rust", src.as_bytes()).unwrap(),
            )
            .unwrap();

        // NO rails_edges row targets "a.rs" at all — nothing to find.
        let out = usages_at(&store, &repo, repo_id, "a.rs", 1, 3, None, 500).unwrap();
        assert!(
            !out.likely
                .iter()
                .any(|r| r.path == "app/controllers/x_controller.rb"),
            "{:#?}",
            out.likely
        );
        assert!(
            !out.candidate
                .iter()
                .any(|r| r.path == "app/controllers/x_controller.rb"),
            "{:#?}",
            out.candidate
        );
    }

    #[test]
    fn reverse_framework_lookup_surfaces_inbound_edges_on_a_destination_only_locale_yml() {
        // `config/locales/*.yml` is a pure DESTINATION — never a rails-lens
        // extraction SOURCE (`rails_lens_relevant_path`'s own doc) — but it
        // demonstrably holds inbound `i18n_key` rows.
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        let (root, repo, repo_id) = fixture_repo(&store, "fixture");

        let yml_path = "config/locales/en.yml";
        write_file(root.path(), yml_path, "en:\n  hello: \"Hi\"\n");

        store
            .replace_rails_edges(
                repo_id,
                "app/models/x.rb",
                "blobhash-i18n",
                crate::frameworks::RAILS_LENS_GRAMMAR_VERSION,
                &[crate::frameworks::FrameworkEdge {
                    kind: crate::frameworks::EdgeKind::I18nKey,
                    src_path: "app/models/x.rb".to_string(),
                    src_line: Some(4),
                    src_symbol: None,
                    dst_kind: Some("locale_file".to_string()),
                    dst_path: Some(yml_path.to_string()),
                    dst_symbol: Some("hello".to_string()),
                    trust: crate::frameworks::Trust::Likely,
                    extra_json: None,
                }],
            )
            .unwrap();

        let out = usages_at(&store, &repo, repo_id, yml_path, 1, 1, None, 500).unwrap();
        assert!(
            out.likely
                .iter()
                .any(|r| r.path == "app/models/x.rb" && r.line == 4),
            "{:#?}",
            out.likely
        );
    }

    #[test]
    fn reverse_framework_lookup_surfaces_inbound_edges_on_a_destination_only_stimulus_js() {
        // `app/javascript/**/controllers/*_controller.js` is likewise a
        // pure DESTINATION for `stimulus_binding` edges.
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        let (root, repo, repo_id) = fixture_repo(&store, "fixture");

        let js_path = "app/javascript/controllers/widget_controller.js";
        write_file(root.path(), js_path, "export default class {}\n");

        store
            .replace_rails_edges(
                repo_id,
                "app/views/x/show.html.erb",
                "blobhash-stimulus",
                crate::frameworks::RAILS_LENS_GRAMMAR_VERSION,
                &[crate::frameworks::FrameworkEdge {
                    kind: crate::frameworks::EdgeKind::StimulusBinding,
                    src_path: "app/views/x/show.html.erb".to_string(),
                    src_line: Some(2),
                    src_symbol: Some("widget".to_string()),
                    dst_kind: Some("js_controller".to_string()),
                    dst_path: Some(js_path.to_string()),
                    dst_symbol: None,
                    trust: crate::frameworks::Trust::Likely,
                    extra_json: None,
                }],
            )
            .unwrap();

        let out = usages_at(&store, &repo, repo_id, js_path, 1, 1, None, 500).unwrap();
        assert!(
            out.likely
                .iter()
                .any(|r| r.path == "app/views/x/show.html.erb" && r.line == 2),
            "{:#?}",
            out.likely
        );
    }
}
