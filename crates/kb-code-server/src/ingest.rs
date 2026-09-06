//! Ingest: turns raw blob bytes into `files`/`symbols`/`highlights` rows
//! (ADR-2: the derived rows are keyed by `(blob_hash, salt)`, so this is
//! the ONLY place kb-code decides whether to parse or skip a cache hit).
//!
//! Two entry points:
//! - [`index_file`] — the primitive: one blob's bytes in, a `files` row
//!   (and, if parseable and not already cached, fresh `symbols`/
//!   `highlights` rows) out. Language-agnostic caller contract; doesn't
//!   care where the bytes came from.
//! - [`index_repo_working_tree`] — walks a repo's git tree at `rev` (via
//!   the W1.3 `git` module) and calls `index_file` for every tracked
//!   regular file. **Seam note**: this reads the tree at a REV (`HEAD` by
//!   default), not the live filesystem working tree — the W1.4 watcher
//!   (in flight separately) is what will eventually feed live,
//!   possibly-uncommitted paths. When it lands, it will need its own
//!   git-compatible content-hash routine (`blob <len>\0<content>`, sha1)
//!   to compute a `blob_hash` for bytes that aren't yet committed — no
//!   such routine exists in kb-code today, because nothing calls
//!   `index_file` with a non-git-derived hash in this Wave (every blob
//!   hash below comes straight from `TreeEntry::oid`, which git already
//!   computed during the tree walk). That's a deliberate deferral, not an
//!   oversight.
//!
//! Because `git ls-tree` only ever lists tracked paths, gitignored files
//! are inherently excluded — no separate gitignore evaluation needed.
//!
//! ## Content caps (checked in this order)
//! 1. **>5 MiB** ([`MAX_PARSE_BYTES`]) → `files` row only, `lang =
//!    "too-large"`.
//! 2. **Git LFS pointer** (bytes start with `"version https://git-lfs"`) →
//!    `files` row only, `lang = "lfs"`.
//! 3. **Non-UTF8** → `files` row only, `lang = "binary"`.
//! 4. **No grammar for this file type** ([`lang::detect`] returns `None`)
//!    → `files` row only, `lang = "unknown"`.
//!
//! Anything past all four gets parsed (or served from the blob-hash cache)
//! according to its `syntax/1` extraction TIER (`crate::syntax`): `Full`
//! derives symbols and highlight spans, `HighlightOnly` derives spans and
//! skips symbols by ONE short-circuit, `None` (a parse-only grammar like
//! ERB) derives neither. The tier never aborts the walk — the `files` row
//! and the derived rows are written either way, an empty set being the
//! honest answer rather than a missing one. Note that `IngestOutcome.tier`
//! and the four `TIER_*` constants above are a DIFFERENT axis (content
//! skip markers in `files.lang`) from `syntax::Tier` (the file type's
//! extraction tier); see `syntax`'s module doc.

use crate::extract;
use crate::git::{EntryKind, GitError, GitRepo};
use crate::highlight;
use crate::lang::{self, LangError};
use crate::store::{NewComment, Store, StoreError};

#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Lang(#[from] LangError),
    #[error(transparent)]
    Git(#[from] GitError),
}

pub type Result<T> = std::result::Result<T, IngestError>;

/// Content-size parse cap — see the module doc's cap ordering.
pub const MAX_PARSE_BYTES: u64 = 5 * 1024 * 1024;

/// Git-compatible blob content hash: `sha1("blob {len}\0" + content)`,
/// hex-encoded — bit-for-bit what `git hash-object <file>` prints (pinned by
/// `git_blob_hash_matches_git_hash_object` below). W1.5's tree-walk never
/// needed this (every blob hash there comes straight from `TreeEntry::oid`,
/// which git already computed) — this is the missing routine that seam's
/// doc flagged: the live-mirror sink (`sink.rs`) observes WORKING-TREE bytes
/// that may not be committed at all, so there's no ODB entry to read an oid
/// from; hashing the bytes ourselves, the same way git would, is what lets
/// `store::Store`'s blob-hash-keyed cache (ADR-2) treat a live edit and an
/// eventual `git commit` of the identical content as the SAME cache slot —
/// no re-parse when a working-tree edit is later committed unchanged.
pub fn git_blob_hash(bytes: &[u8]) -> String {
    use sha1::{Digest, Sha1};
    let mut hasher = Sha1::new();
    hasher.update(b"blob ");
    hasher.update(bytes.len().to_string());
    hasher.update(b"\0");
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

const LFS_POINTER_PREFIX: &[u8] = b"version https://git-lfs";

pub const TIER_UNKNOWN: &str = "unknown";
pub const TIER_BINARY: &str = "binary";
pub const TIER_TOO_LARGE: &str = "too-large";
pub const TIER_LFS: &str = "lfs";

/// What the HIGHLIGHT cache gate did for one file (V72-H2b, D7). A
/// SEPARATE axis from the symbol gate: a `highlight_salt` bump re-paints
/// without re-extracting a single symbol, and vice versa.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HighlightCache {
    /// The blob already had spans under the CURRENT `highlight_salt`.
    Hit,
    /// Painted now, and stored.
    Miss,
    /// This file TYPE derives no spans (`syntax::Tier::None` — a
    /// parse-only grammar like ERB). Decided before the cache is
    /// consulted, because it is a property of the type, not of the blob.
    SkippedTier,
}

impl HighlightCache {
    /// The wire/CLI value (`GET /api/file`'s `highlight_cache`).
    pub fn as_str(self) -> &'static str {
        match self {
            HighlightCache::Hit => "hit",
            HighlightCache::Miss => "miss",
            HighlightCache::SkippedTier => "skipped_tier",
        }
    }
}

/// The result of one [`index_file`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngestOutcome {
    /// `true` if `(blob_hash, symbol_salt)` was already DERIVED — no
    /// symbol extraction happened. V72-H2b: this is the marker's
    /// existence, not a row count, so a blob whose honest symbol set is
    /// empty is a cache hit on its second visit like anything else.
    pub cache_hit: bool,
    /// The language id ("rust"/"python"/"ruby") if parsed/cached, else one
    /// of the `TIER_*` constants.
    pub tier: &'static str,
    pub symbol_count: usize,
    /// V72-H2b — what the independent HIGHLIGHT gate did. `SkippedTier`
    /// for every content-capped file too (nothing is painted when nothing
    /// is parsed).
    pub highlight_cache: HighlightCache,
}

/// Index one file's bytes at `path` (source-relative) into `repo_id`.
/// `blob_hash` is supplied by the caller — see the module doc's seam note
/// on where it comes from today (always a git tree-walk oid).
///
/// `occurrences_enabled` (B5a) gates ONLY the occurrences derivation pass
/// below — every caller resolves it ONCE per repo via `config::
/// OccurrencesSection::repo_enabled(repo_name)` before calling in (this fn
/// only knows `repo_id`, not the repo's configured NAME, so it can't
/// re-derive the check itself). `false` never affects `files`/`symbols`/
/// `highlights` at all — those are unconditional, same as before B5a; a
/// disabled repo's blobs simply never gain `occurrences` rows, and
/// `resolve.rs` transparently falls back to its existing word-scan path.
///
/// `is_rails` (PRR-N3) gates the Rails-lens dispatch (`crate::frameworks`)
/// the SAME way: every caller resolves it ONCE per repo (`frameworks::
/// rails::detect_is_rails` + `config::RailsLensSection::repo_enabled`,
/// cached — see that fn's doc for why it must never re-run per file) and
/// passes the resolved bool straight through. `false` (the default for
/// every non-Rails repo — kb/kb-code/research/demo-repo) means this fn never
/// even checks whether `path` LOOKS like a routes/controller/view file, so
/// those repos pay zero added cost.
#[allow(clippy::too_many_arguments)]
pub fn index_file(
    store: &Store,
    repo_id: i64,
    path: &str,
    bytes: &[u8],
    blob_hash: &str,
    occurrences_enabled: bool,
    is_rails: bool,
    comment_keywords: &crate::comments::KeywordSet,
) -> Result<IngestOutcome> {
    let size = bytes.len() as u64;

    if size > MAX_PARSE_BYTES {
        store.upsert_file(repo_id, path, blob_hash, TIER_TOO_LARGE, size)?;
        return Ok(IngestOutcome {
            cache_hit: false,
            tier: TIER_TOO_LARGE,
            symbol_count: 0,
            highlight_cache: HighlightCache::SkippedTier,
        });
    }
    if bytes.starts_with(LFS_POINTER_PREFIX) {
        store.upsert_file(repo_id, path, blob_hash, TIER_LFS, size)?;
        return Ok(IngestOutcome {
            cache_hit: false,
            tier: TIER_LFS,
            symbol_count: 0,
            highlight_cache: HighlightCache::SkippedTier,
        });
    }
    if std::str::from_utf8(bytes).is_err() {
        store.upsert_file(repo_id, path, blob_hash, TIER_BINARY, size)?;
        return Ok(IngestOutcome {
            cache_hit: false,
            tier: TIER_BINARY,
            symbol_count: 0,
            highlight_cache: HighlightCache::SkippedTier,
        });
    }
    let Some(lang_info) = lang::detect(path, Some(bytes)) else {
        store.upsert_file(repo_id, path, blob_hash, TIER_UNKNOWN, size)?;
        return Ok(IngestOutcome {
            cache_hit: false,
            tier: TIER_UNKNOWN,
            symbol_count: 0,
            highlight_cache: HighlightCache::SkippedTier,
        });
    };

    // The `files` pointer row is written unconditionally, even on a cache
    // hit: a DIFFERENT path sharing this blob_hash still needs its own
    // (repo_id, path) row pointing at the shared derived data.
    store.upsert_file(repo_id, path, blob_hash, lang_info.id, size)?;

    // V72-H1 (D7) — the syntax/1 extraction TIER, as ONE short-circuit at
    // the top of the derivation. A `HighlightOnly` row writes spans and
    // an explicitly EMPTY symbol set; a `None` row (a parse-only grammar
    // like ERB) writes neither. Never an aborted walk: the `files` row is
    // already written above, the rows below are written either way, and
    // nothing here can return an error a whole-repo walk would propagate.
    //
    // NOTE the name collision this crate lives with: `IngestOutcome.tier`
    // (and the `TIER_*` consts) are the CONTENT skip markers stored in
    // `files.lang`; `syntax::Tier` is the file TYPE's extraction tier.
    // Different axes — see `syntax`'s module doc.
    let plan = crate::syntax::row_for_path(path, Some(bytes))
        .map(|row| row.plan())
        .unwrap_or(crate::syntax::Plan {
            highlight: false,
            symbols: false,
        });

    // V72-H2b (D7) — TWO independent gates, one per salt family. Before
    // this unit the highlight pass lived INSIDE the symbols miss branch,
    // so a `highlights.scm`/role-table change could only be shipped by
    // re-extracting every symbol in the corpus, and a `tags.scm` fix
    // re-painted every file. Each gate now asks about its own family's
    // salt and its own marker.
    //
    // Both gates read `Store::is_derived` — the MARKER's existence, never
    // a row count. `has_symbols`'s `COUNT(*) > 0` made the cache-hit
    // branch structurally unreachable for every blob whose honest
    // derivation is empty (V72-H1 reported it for ERB; it was equally true
    // of an SCSS file, a comment-only Rust file and a heading-less
    // Markdown file), so those re-parsed on every single visit.
    let symbol_salt = lang_info.symbol_salt;
    let highlight_salt = lang_info.highlight_salt;

    let outcome = if store.is_derived(blob_hash, lang::SaltFamily::Symbol, symbol_salt)? {
        let symbol_count = store.symbols_for_blob(blob_hash, symbol_salt)?.len();
        IngestOutcome {
            cache_hit: true,
            tier: lang_info.id,
            symbol_count,
            // Overwritten by the highlight gate below; this value is never
            // observed.
            highlight_cache: HighlightCache::SkippedTier,
        }
    } else {
        let symbols = if plan.symbols {
            extract::extract_symbols(lang_info.id, bytes)?
        } else {
            Vec::new()
        };
        let symbol_count = symbols.len();
        store.replace_symbols(blob_hash, symbol_salt, &symbols)?;
        IngestOutcome {
            cache_hit: false,
            tier: lang_info.id,
            symbol_count,
            highlight_cache: HighlightCache::SkippedTier,
        }
    };

    // The HIGHLIGHT gate. `SkippedTier` is decided FIRST and from the plan
    // alone — it is a property of the file TYPE, so it stays the answer on
    // the second visit as much as the first. The empty row is still
    // written once for such a type, which is what keeps `GET /api/file`
    // answering `Some([])` (rather than `null`) for a parse-only grammar,
    // exactly as it did before this unit.
    let highlight_cache = if !plan.highlight {
        if !store.is_derived(blob_hash, lang::SaltFamily::Highlight, highlight_salt)? {
            store.put_highlights(blob_hash, highlight_salt, &[])?;
        }
        HighlightCache::SkippedTier
    } else if store.is_derived(blob_hash, lang::SaltFamily::Highlight, highlight_salt)? {
        HighlightCache::Hit
    } else {
        let spans = highlight::extract_highlights(lang_info.id, bytes)?;
        store.put_highlights(blob_hash, highlight_salt, &spans)?;
        HighlightCache::Miss
    };
    let outcome = IngestOutcome {
        highlight_cache,
        ..outcome
    };

    // B2 — occurrences: a SEPARATE cache-hit check (`has_occurrences`, not
    // `has_symbols`) so a blob whose symbols/highlights were already cached
    // before this pass existed still gets its occurrences derived, and vice
    // versa (see `occurrences.rs`'s module doc's "Cache key" section).
    // Gated to the token-level languages AND (B5a) `occurrences_enabled`;
    // never affects `outcome` above — this is a parallel, additive pass,
    // not a replacement for the symbols cache decision.
    if occurrences_enabled
        && lang::supports_token_level(lang_info.id)
        && !store.has_occurrences(blob_hash, symbol_salt)?
    {
        let occurrences = crate::occurrences::extract_occurrences(lang_info.id, bytes)?;
        store.replace_occurrences(blob_hash, symbol_salt, &occurrences)?;
    }

    // V72-J1 — `comments/1`: the comment index, which SUBSUMES the Phase-N
    // TODO index (`todo_items` and `extract_todos` are gone;
    // `GET /api/todos` is a filtered view over these rows). Path-keyed with
    // a `(blob_sha, comments_version)` freshness stamp, so an unchanged
    // file skips the tree-sitter pass entirely — strictly cheaper than the
    // pass it replaces, which re-parsed on every visit. Deliberately NOT
    // inside the `if let Some(file_id)` block: these rows key on
    // `(repo_id, path)`, never on `files.id`.
    //
    // V72-H2b — `comments/1` rides the SYMBOL salt, and is NOT
    // double-keyed. `comments_version_for` already folds a salt together
    // with the keyword grammar into its own freshness stamp, so the
    // question is only WHICH salt: comment extraction reads the symbol
    // rows (`symbols_for_blob`, for attachment) and never a highlight
    // span, so a highlight-query or role-table bump must not invalidate a
    // single comment row. Passing `highlight_salt` here would re-extract
    // every comment in the corpus for a change that cannot alter one.
    let comments_version =
        crate::comments::comments_version_for(lang_info.symbol_salt, comment_keywords);
    if !store.has_comments(repo_id, path, blob_hash, &comments_version)? {
        let symbols = store.symbols_for_blob(blob_hash, symbol_salt)?;
        let extraction =
            crate::comments::extract_comments(lang_info.id, bytes, &symbols, comment_keywords)?;
        let rows: Vec<NewComment> = extraction
            .blocks
            .iter()
            .map(crate::comments::to_new_comment)
            .collect();
        store.replace_comments(repo_id, path, blob_hash, &comments_version, &rows)?;
    }

    if let Some(file_id) = store.file_id(repo_id, path)? {
        // V3.G2 — import graph: content-addressed specs (cache by
        // blob_hash+salt) + repo-addressed edges rebuilt every visit
        // (resolution depends on the live files table).
        if crate::imports::supports(lang_info.id) {
            let specs = if store.has_import_specs(blob_hash, symbol_salt)? {
                store.import_specs_for_blob(blob_hash, symbol_salt)?
            } else {
                let specs = crate::import_graph::extract_import_specs(lang_info.id, bytes);
                store.replace_import_specs(blob_hash, symbol_salt, &specs)?;
                specs
            };
            // Resolve against the repo root. The caller always indexes
            // with a known repo path via the working-tree walk / sink;
            // we look up the root from the files/repos join.
            if let Some(repo_root) = store.repo_root(repo_id)? {
                let repo_root = std::path::PathBuf::from(repo_root);
                let resolved = crate::import_graph::resolve_import_edges(
                    &repo_root,
                    std::path::Path::new(path),
                    lang_info.id,
                    &specs,
                );
                let mut edges: Vec<(String, i64)> = Vec::new();
                for (raw_spec, target_rel) in resolved {
                    if let Some(tid) = store.file_id(repo_id, &target_rel)? {
                        edges.push((raw_spec, tid));
                    }
                }
                store.replace_import_edges(file_id, &edges)?;
            }
        }

        // V3.1-H1 — call sites + type relations (content-addressed, same
        // salt as symbols; four proof languages only).
        if crate::hierarchy::supports_hierarchy(lang_info.id) {
            if !store.has_call_sites(blob_hash, symbol_salt)? {
                let symbols = store.symbols_for_blob(blob_hash, symbol_salt)?;
                let sites = crate::hierarchy::extract_call_sites(lang_info.id, bytes, &symbols);
                store.replace_call_sites(blob_hash, symbol_salt, &sites)?;
            }
            if !store.has_type_relations(blob_hash, symbol_salt)? {
                let rels = crate::hierarchy::extract_type_relations(lang_info.id, bytes);
                store.replace_type_relations(blob_hash, symbol_salt, &rels)?;
            }
        }
    }

    // PRR-N3/N4 — Rails lens: gated on BOTH `is_rails` (cheap, resolved once
    // per repo by the caller) AND a path-pattern check
    // (`frameworks::rails_lens_relevant_path` — config/routes* /
    // app/controllers/** / app/views/** / app/components/** / app/models/**
    // / app/jobs/** / app/mailers/** / spec/**) so every OTHER file in a
    // Rails repo — assets, db/schema.rb, … — never even reaches
    // `frameworks::extract_edges`. `app/javascript/controllers/**` and
    // `config/locales/**` are deliberately NOT in that gate — they're pure
    // stimulus/i18n DESTINATIONS, resolved by a direct repo-root filesystem
    // walk from the SOURCE side, never an extraction source themselves (see
    // `frameworks::rails_lens_relevant_path`'s own doc). Deliberately NOT
    // inside the `if let Some(file_id)` block above: rails-lens rows don't
    // need a `file_id`, only `repo_id` + this blob's own `path`/`blob_hash`
    // (see `Store::replace_rails_edges`'s doc for why there's no cache-hit
    // skip here, unlike `call_sites`/`type_relations`, and for why the
    // replace-time delete is scoped to `(repo_id, path)` — R1).
    if is_rails && crate::frameworks::rails_lens_relevant_path(path) {
        if let Some(repo_root) = store.repo_root(repo_id)? {
            let repo_root = std::path::PathBuf::from(repo_root);
            let edges = crate::frameworks::extract_edges(&repo_root, path, bytes);
            store.replace_rails_edges(
                repo_id,
                path,
                blob_hash,
                crate::frameworks::RAILS_LENS_GRAMMAR_VERSION,
                &edges,
            )?;
        }
    }

    // V71-G0 — the entity index (`entities/1`). Gated FIRST on the
    // language (`crate::entities::indexes_lang`, Ruby only), so a non-Ruby
    // repo pays exactly one string comparison per file and never reaches
    // the store or the config cache below. Unlike the Rails lens above
    // this needs no `is_rails` flag from the caller: a Ruby file is a Ruby
    // file, and a tree with no Zeitwerk configuration degrades honestly
    // (`entities::zeitwerk::STATE_DEGRADED`) instead of being skipped.
    //
    // Re-derived on EVERY visit, with no cache-hit skip — the same reason
    // `replace_rails_edges` has none: these rows are keyed by (repo,
    // worktree, path), not by blob, because an entity's FQN depends on the
    // path and the checkout's own config, not on the bytes alone. The
    // added per-file cost is one `symbols_for_blob` read (the symbols were
    // extracted or cache-read just above; this re-reads them rather than
    // threading them out of both branches) plus a cached config lookup —
    // no second tree-sitter parse. `bytes` is passed for ONE reason: the
    // Ruby tags query drops the scope of a compact `class A::B`, and
    // `defs_for_file` recovers it from the definition's own source line.
    if crate::entities::indexes_lang(lang_info.id) {
        if let Some(repo_root) = store.repo_root(repo_id)? {
            let repo_root = std::path::PathBuf::from(repo_root);
            let symbols = store.symbols_for_blob(blob_hash, symbol_salt)?;
            let zeitwerk = crate::entities::zeitwerk::zeitwerk_for(&repo_root);
            let worktree = crate::entities::worktree_key_for(&repo_root);
            let defs = crate::entities::defs_for_file(path, bytes, &symbols, &zeitwerk);
            store.replace_entity_defs(
                repo_id,
                &worktree,
                path,
                blob_hash,
                zeitwerk.state,
                &defs,
            )?;
        }
    }

    Ok(outcome)
}

/// Aggregate counts from one [`index_repo_working_tree`] walk.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct WalkStats {
    /// Every regular file visited (symlinks/submodules are not counted —
    /// see the module doc).
    pub files: usize,
    /// Freshly parsed (not a cache hit, and not a skipped tier).
    pub parsed: usize,
    pub cache_hits: usize,
    /// Content-capped: too-large / binary / lfs / unknown-extension.
    pub skipped_tier: usize,
    /// Sum of `IngestOutcome::symbol_count` across every file visited.
    pub symbols: usize,
    /// V72-H2b — the independent HIGHLIGHT gate's own tally. These three
    /// sum to `files`, and they are what makes a `highlight_salt` bump's
    /// cost visible in the boot log rather than inferred from wall clock.
    pub highlight_hits: usize,
    pub highlight_misses: usize,
    pub highlight_skipped: usize,
}

impl WalkStats {
    fn record_highlight(&mut self, cache: HighlightCache) {
        match cache {
            HighlightCache::Hit => self.highlight_hits += 1,
            HighlightCache::Miss => self.highlight_misses += 1,
            HighlightCache::SkippedTier => self.highlight_skipped += 1,
        }
    }
}

/// Walk `repo`'s git tree at `rev` (default caller passes `"HEAD"`) and
/// `index_file` every regular file. See the module doc's seam note: this
/// is a REV-based walk (object-database reads only), not a live-filesystem
/// walk. `occurrences_enabled` (B5a) is resolved ONCE by the caller (per
/// repo) and threaded straight through to every `index_file` call this walk
/// makes — see that fn's own doc.
pub fn index_repo_working_tree(
    store: &Store,
    repo: &GitRepo,
    repo_id: i64,
    rev: &str,
    occurrences_enabled: bool,
    is_rails: bool,
    comment_keywords: &crate::comments::KeywordSet,
) -> Result<WalkStats> {
    let mut stats = WalkStats::default();
    walk_dir(
        store,
        repo,
        repo_id,
        rev,
        "",
        occurrences_enabled,
        is_rails,
        &mut stats,
        comment_keywords,
    )?;
    // V3.G2 — second pass: rebuild import edges now that every files row
    // exists. Per-file edge resolution during the walk can miss targets
    // indexed later; this pass makes the graph complete for a full walk.
    rebuild_import_edges_for_repo(store, repo_id)?;
    Ok(stats)
}

/// Re-resolve import edges for every file in `repo_id` that has a
/// content-addressed import_specs set. Safe to call after a full walk or
/// after a bulk reindex; cheap when specs are empty.
pub fn rebuild_import_edges_for_repo(store: &Store, repo_id: i64) -> Result<()> {
    let Some(repo_root) = store.repo_root(repo_id)? else {
        return Ok(());
    };
    let repo_root = std::path::PathBuf::from(repo_root);
    let files = store.list_files(repo_id)?;
    for f in &files {
        if !crate::imports::supports(&f.lang) {
            continue;
        }
        let Some(lang_info) = lang::for_id(&f.lang) else {
            continue;
        };
        let Some(file_id) = store.file_id(repo_id, &f.path)? else {
            continue;
        };
        let specs = store.import_specs_for_blob(&f.blob_hash, lang_info.symbol_salt)?;
        if specs.is_empty() && !store.has_import_specs(&f.blob_hash, lang_info.symbol_salt)? {
            continue;
        }
        let resolved = crate::import_graph::resolve_import_edges(
            &repo_root,
            std::path::Path::new(&f.path),
            &f.lang,
            &specs,
        );
        let mut edges: Vec<(String, i64)> = Vec::new();
        for (raw_spec, target_rel) in resolved {
            if let Some(tid) = store.file_id(repo_id, &target_rel)? {
                edges.push((raw_spec, tid));
            }
        }
        store.replace_import_edges(file_id, &edges)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn walk_dir(
    store: &Store,
    repo: &GitRepo,
    repo_id: i64,
    rev: &str,
    dir_path: &str,
    occurrences_enabled: bool,
    is_rails: bool,
    stats: &mut WalkStats,
    comment_keywords: &crate::comments::KeywordSet,
) -> Result<()> {
    for entry in repo.list_tree(rev, dir_path)? {
        let full_path = if dir_path.is_empty() {
            entry.name.clone()
        } else {
            format!("{dir_path}/{}", entry.name)
        };
        match entry.kind {
            EntryKind::Dir => walk_dir(
                store,
                repo,
                repo_id,
                rev,
                &full_path,
                occurrences_enabled,
                is_rails,
                stats,
                comment_keywords,
            )?,
            EntryKind::File => match repo.read_blob(rev, &full_path, MAX_PARSE_BYTES) {
                Ok(bytes) => {
                    let outcome = index_file(
                        store,
                        repo_id,
                        &full_path,
                        &bytes,
                        &entry.oid,
                        occurrences_enabled,
                        is_rails,
                        comment_keywords,
                    )?;
                    stats.files += 1;
                    stats.symbols += outcome.symbol_count;
                    stats.record_highlight(outcome.highlight_cache);
                    if outcome.cache_hit {
                        stats.cache_hits += 1;
                    } else if lang::for_id(outcome.tier).is_some() {
                        stats.parsed += 1;
                    } else {
                        stats.skipped_tier += 1;
                    }
                }
                // The git layer's own cap tripped first (its default is
                // 10 MiB vs our 5 MiB parse cap, but we pass MAX_PARSE_BYTES
                // explicitly so this is really the same boundary) — the
                // error still carries the real size, so the files row is
                // just as accurate as the in-process size check's branch.
                Err(GitError::TooLarge { size, .. }) => {
                    store.upsert_file(repo_id, &full_path, &entry.oid, TIER_TOO_LARGE, size)?;
                    stats.files += 1;
                    stats.skipped_tier += 1;
                    stats.record_highlight(HighlightCache::SkippedTier);
                }
                Err(e) => return Err(e.into()),
            },
            // Wave-1 scope is regular file content — symlinks and
            // submodule pins are not indexed as files at all.
            EntryKind::Symlink | EntryKind::Submodule => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    /// Every ingest test indexes with the SHIPPED default keyword set —
    /// `[comments] keywords` is a per-deployment override, not a test knob.
    fn kw() -> crate::comments::KeywordSet {
        crate::comments::KeywordSet::defaults()
    }

    use super::*;
    use std::path::Path;
    use std::process::Command;

    fn open_store() -> (tempfile::TempDir, Store) {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        (tmp, store)
    }

    const RUST_SRC: &[u8] = b"fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n";

    fn real_git_hash_object(bytes: &[u8]) -> String {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("blob");
        std::fs::write(&path, bytes).unwrap();
        let out = Command::new("git")
            .arg("hash-object")
            .arg(&path)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git hash-object failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    #[test]
    fn git_blob_hash_matches_git_hash_object() {
        for bytes in [
            &b""[..],
            b"hello world\n",
            RUST_SRC,
            b"no trailing newline",
            &[0xffu8, 0x00, 0xfe, 0x01, 0x02][..], // embedded NUL + non-UTF8 bytes
        ] {
            let want = real_git_hash_object(bytes);
            let got = git_blob_hash(bytes);
            assert_eq!(got, want, "mismatch for {bytes:?}");
            // sha1 hex digest is always 40 lowercase hex chars.
            assert_eq!(got.len(), 40);
            assert!(got.chars().all(|c| c.is_ascii_hexdigit()));
        }
    }

    #[test]
    fn git_blob_hash_matches_a_real_committed_blob_oid() {
        // Cross-check against a real repo's own TreeEntry::oid (the git
        // module's own source of truth for a COMMITTED blob) rather than
        // just the `git hash-object` CLI — same content, same hash, via a
        // completely independent code path (gix's ODB write during `git
        // add`/`git commit`, not the `hash-object` plumbing command).
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["init", "-q", "-b", "main"])
            .output()
            .expect("git runs");
        assert!(out.status.success());
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["config", "user.email", "t@example.com"])
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["config", "user.name", "T"])
            .output()
            .unwrap();
        std::fs::write(dir.join("f.txt"), RUST_SRC).unwrap();
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["add", "-A"])
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["commit", "-q", "-m", "c1"])
            .output()
            .unwrap();

        let repo = GitRepo::open(dir).unwrap();
        let entries = repo.list_tree("HEAD", "").unwrap();
        let entry = entries.iter().find(|e| e.name == "f.txt").unwrap();
        assert_eq!(git_blob_hash(RUST_SRC), entry.oid);
    }

    #[test]
    fn parses_a_supported_language_on_first_sight() {
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        let outcome = index_file(
            &store,
            repo_id,
            "src/lib.rs",
            RUST_SRC,
            "hashA",
            true,
            false,
            &kw(),
        )
        .unwrap();
        assert!(!outcome.cache_hit);
        assert_eq!(outcome.tier, "rust");
        assert_eq!(outcome.symbol_count, 1);

        let file = store.get_file(repo_id, "src/lib.rs").unwrap().unwrap();
        assert_eq!(file.blob_hash, "hashA");
        assert_eq!(file.lang, "rust");
    }

    #[test]
    fn identical_bytes_indexed_twice_is_a_cache_hit_with_zero_reparse() {
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        let first = index_file(
            &store,
            repo_id,
            "src/lib.rs",
            RUST_SRC,
            "hashA",
            true,
            false,
            &kw(),
        )
        .unwrap();
        assert!(!first.cache_hit);

        let second = index_file(
            &store,
            repo_id,
            "src/lib.rs",
            RUST_SRC,
            "hashA",
            true,
            false,
            &kw(),
        )
        .unwrap();
        assert!(
            second.cache_hit,
            "identical (blob_hash, salt) must be a cache hit"
        );
        assert_eq!(second.symbol_count, first.symbol_count);
    }

    // --- B2: occurrences wiring ---------------------------------------------

    #[test]
    fn index_file_derives_occurrences_for_a_token_level_language() {
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        index_file(
            &store,
            repo_id,
            "src/lib.rs",
            RUST_SRC,
            "hashA",
            true,
            false,
            &kw(),
        )
        .unwrap();
        assert!(store
            .has_occurrences("hashA", lang::RUST.symbol_salt)
            .unwrap());
        let occs = store
            .occurrences_for_blob("hashA", lang::RUST.symbol_salt)
            .unwrap();
        assert!(
            !occs.is_empty(),
            "expected at least one occurrence: {occs:?}"
        );
    }

    #[test]
    fn index_file_skips_occurrences_for_a_non_token_level_language() {
        // YAML (a CST-walk key-path outline, ADR-7) is NOT one of
        // `lang::TOKEN_LEVEL_LANG_IDS` even after B5b widened it to eight
        // languages — see that const's own doc for why YAML/TOML/JSON stay
        // out permanently, not just unmeasured.
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        index_file(
            &store,
            repo_id,
            "config.yaml",
            b"key: value\n",
            "hashYaml",
            true,
            false,
            &kw(),
        )
        .unwrap();
        assert!(!store
            .has_occurrences("hashYaml", lang::YAML.symbol_salt)
            .unwrap());
    }

    // --- B5a: `[occurrences]` config gate -----------------------------------

    #[test]
    fn index_file_skips_occurrences_when_disabled_for_this_repo() {
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        let outcome = index_file(
            &store,
            repo_id,
            "src/lib.rs",
            RUST_SRC,
            "hashA",
            false,
            false,
            &kw(),
        )
        .unwrap();
        // The occurrences gate never affects files/symbols/highlights —
        // only occurrences derivation.
        assert!(!outcome.cache_hit);
        assert_eq!(outcome.symbol_count, 1);
        assert!(!store
            .has_occurrences("hashA", lang::RUST.symbol_salt)
            .unwrap());
        let file = store.get_file(repo_id, "src/lib.rs").unwrap().unwrap();
        assert_eq!(file.lang, "rust");
    }

    #[test]
    fn index_file_derives_occurrences_once_re_enabled_for_the_same_blob() {
        // A blob indexed with occurrences OFF, then re-indexed (same
        // blob_hash) with occurrences ON, must derive them — the cache-hit
        // check is `has_occurrences`, which is still `false` after the
        // first (disabled) call.
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        index_file(
            &store,
            repo_id,
            "src/lib.rs",
            RUST_SRC,
            "hashA",
            false,
            false,
            &kw(),
        )
        .unwrap();
        assert!(!store
            .has_occurrences("hashA", lang::RUST.symbol_salt)
            .unwrap());

        index_file(
            &store,
            repo_id,
            "src/lib.rs",
            RUST_SRC,
            "hashA",
            true,
            false,
            &kw(),
        )
        .unwrap();
        assert!(store
            .has_occurrences("hashA", lang::RUST.symbol_salt)
            .unwrap());
    }

    #[test]
    fn index_file_does_not_re_derive_occurrences_on_a_second_call() {
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        index_file(
            &store,
            repo_id,
            "src/lib.rs",
            RUST_SRC,
            "hashA",
            true,
            false,
            &kw(),
        )
        .unwrap();
        let first = store
            .occurrences_for_blob("hashA", lang::RUST.symbol_salt)
            .unwrap();

        // Manually corrupt the cached rows in a way a re-derive would fix —
        // proves the second `index_file` call left them alone (the
        // `has_occurrences` cache-hit check short-circuited it).
        store
            .replace_occurrences(
                "hashA",
                lang::RUST.symbol_salt,
                &[crate::occurrences::Occurrence {
                    ordinal: 0,
                    name: "sentinel".to_string(),
                    role: "def".to_string(),
                    line: 1,
                    col_start: 0,
                    col_end: 1,
                    source: crate::occurrences::SOURCE_TS.to_string(),
                    local_def_ordinal: None,
                }],
            )
            .unwrap();
        index_file(
            &store,
            repo_id,
            "src/lib.rs",
            RUST_SRC,
            "hashA",
            true,
            false,
            &kw(),
        )
        .unwrap();
        let after = store
            .occurrences_for_blob("hashA", lang::RUST.symbol_salt)
            .unwrap();
        assert_ne!(after, first, "the sentinel row must survive untouched");
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].name, "sentinel");
    }

    #[test]
    fn changed_bytes_under_the_same_path_re_derive() {
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        index_file(
            &store,
            repo_id,
            "src/lib.rs",
            RUST_SRC,
            "hashA",
            true,
            false,
            &kw(),
        )
        .unwrap();

        let changed = b"fn add(a: i32, b: i32, c: i32) -> i32 {\n    a + b + c\n}\nfn extra() {}\n";
        let outcome = index_file(
            &store,
            repo_id,
            "src/lib.rs",
            changed,
            "hashB",
            true,
            false,
            &kw(),
        )
        .unwrap();
        assert!(
            !outcome.cache_hit,
            "a different blob_hash must never cache-hit"
        );
        assert_eq!(outcome.symbol_count, 2);
    }

    #[test]
    fn same_bytes_under_a_different_path_share_derived_rows() {
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        let first = index_file(
            &store,
            repo_id,
            "src/lib.rs",
            RUST_SRC,
            "hashA",
            true,
            false,
            &kw(),
        )
        .unwrap();
        assert!(!first.cache_hit);

        // Same blob_hash, different path (e.g. a copy/rename) — proves the
        // derived rows are shared (ADR-2), not re-parsed.
        let second = index_file(
            &store,
            repo_id,
            "src/copy.rs",
            RUST_SRC,
            "hashA",
            true,
            false,
            &kw(),
        )
        .unwrap();
        assert!(
            second.cache_hit,
            "same blob_hash under a new path must still cache-hit"
        );

        assert_eq!(store.file_count(repo_id).unwrap(), 2);
        let a = store.get_file(repo_id, "src/lib.rs").unwrap().unwrap();
        let b = store.get_file(repo_id, "src/copy.rs").unwrap().unwrap();
        assert_eq!(a.blob_hash, b.blob_hash);
    }

    #[test]
    fn a_salt_bump_forces_re_derivation() {
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();

        // Simulate a stale cache entry under an OLD salt (as if this blob
        // had been indexed by a previous grammar/query version).
        let stale = crate::extract::Symbol {
            ordinal: 0,
            name: "stale".to_string(),
            kind: "fn".to_string(),
            line_start: 1,
            line_end: 1,
            col_start: 0,
            col_end: 1,
            container: None,
            signature: None,
            doc: None,
            param_min: None,
            param_max: None,
        };
        store
            .replace_symbols("hashA", "rust@0.0.0-fake", &[stale])
            .unwrap();

        // index_file always checks the CURRENT salt (lang::RUST.symbol_salt), so
        // the stale-salt row is invisible to it — a cache miss, real parse.
        let outcome = index_file(
            &store,
            repo_id,
            "src/lib.rs",
            RUST_SRC,
            "hashA",
            true,
            false,
            &kw(),
        )
        .unwrap();
        assert!(
            !outcome.cache_hit,
            "a different salt must not be treated as cached"
        );
        assert_eq!(outcome.symbol_count, 1);

        // V70-H1 — was asserted `== 1` ("untouched, different cache slot
        // entirely") before V70-A3X's `Store::replace_symbols` purge landed.
        // That assertion predates the fix and this crate's OWN new coverage
        // (`store::tests::replace_symbols_purges_only_this_blobs_stale_same_language_rows`)
        // now pins the opposite, intended behaviour: a fresh derivation
        // under the blob's CURRENT salt purges every OTHER stale salt of
        // the SAME language for that blob (`lang_prefix_pattern`) — that's
        // the whole point of the fix (no duplicate symbols in `@` search /
        // `/api/symbols` / `/api/defs` after a real grammar/query bump).
        // This fixture's fake salt shares the "rust" prefix with the real
        // `lang::RUST.symbol_salt` `index_file` just wrote under, so it is exactly
        // the stale row the purge exists to sweep — it must be gone, not
        // "a different cache slot."
        assert!(
            store
                .symbols_for_blob("hashA", "rust@0.0.0-fake")
                .unwrap()
                .is_empty(),
            "a genuine salt bump must purge the stale-salt sibling for the same blob+language"
        );
    }

    #[test]
    fn oversized_content_gets_a_files_row_and_no_symbols() {
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        let big = vec![b'a'; (MAX_PARSE_BYTES + 1) as usize];
        let outcome = index_file(
            &store,
            repo_id,
            "big.rs",
            &big,
            "hashBig",
            true,
            false,
            &kw(),
        )
        .unwrap();
        assert_eq!(outcome.tier, TIER_TOO_LARGE);
        assert_eq!(outcome.symbol_count, 0);
        let file = store.get_file(repo_id, "big.rs").unwrap().unwrap();
        assert_eq!(file.lang, TIER_TOO_LARGE);
        assert_eq!(file.size, MAX_PARSE_BYTES + 1);
        assert!(!store
            .has_symbols("hashBig", lang::RUST.symbol_salt)
            .unwrap());
    }

    #[test]
    fn non_utf8_content_gets_a_files_row_and_no_symbols() {
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        let binary = vec![0xff, 0xfe, 0x00, 0x01, 0x02];
        let outcome = index_file(
            &store,
            repo_id,
            "blob.rs",
            &binary,
            "hashBin",
            true,
            false,
            &kw(),
        )
        .unwrap();
        assert_eq!(outcome.tier, TIER_BINARY);
        let file = store.get_file(repo_id, "blob.rs").unwrap().unwrap();
        assert_eq!(file.lang, TIER_BINARY);
    }

    #[test]
    fn lfs_pointer_gets_a_files_row_and_no_symbols() {
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        let pointer =
            b"version https://git-lfs.github.com/spec/v1\noid sha256:deadbeef\nsize 12345\n";
        let outcome = index_file(
            &store,
            repo_id,
            "big.psd",
            pointer,
            "hashLfs",
            true,
            false,
            &kw(),
        )
        .unwrap();
        assert_eq!(outcome.tier, TIER_LFS);
        let file = store.get_file(repo_id, "big.psd").unwrap().unwrap();
        assert_eq!(file.lang, TIER_LFS);
    }

    /// V72-H2a moved `.md` INTO the registry (it is `markdown` now), so
    /// this test's "an extension nothing claims" case had to move with it
    /// — the property under test is the unregistered-type path, not that
    /// any particular extension is unregistered.
    #[test]
    fn unsupported_extension_gets_a_files_row_and_no_symbols() {
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        let outcome = index_file(
            &store,
            repo_id,
            "NOTES.txt",
            b"hello\n",
            "hashTxt",
            true,
            false,
            &kw(),
        )
        .unwrap();
        assert_eq!(outcome.tier, TIER_UNKNOWN);
        let file = store.get_file(repo_id, "NOTES.txt").unwrap().unwrap();
        assert_eq!(file.lang, TIER_UNKNOWN);
    }

    /// V72-H1 — the `syntax/1` tier, end to end through the pipeline. ERB
    /// is the `Tier::None` row that exists today (a parse-only grammar):
    /// it gets a `files` row under its own lang, and BOTH derived sets are
    /// written EMPTY rather than skipped — the walk is never aborted and a
    /// reader can tell "we looked, and the tier says there is nothing"
    /// from "we never looked".
    #[test]
    fn a_none_tier_language_gets_rows_but_neither_symbols_nor_spans() {
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        let src = b"<h1><%= @order.id %></h1>\n";
        let outcome = index_file(
            &store,
            repo_id,
            "app/views/orders/show.html.erb",
            src,
            "hashErb",
            true,
            false,
            &kw(),
        )
        .unwrap();
        assert_eq!(outcome.tier, "erb");
        assert_eq!(outcome.symbol_count, 0);
        let file = store
            .get_file(repo_id, "app/views/orders/show.html.erb")
            .unwrap()
            .unwrap();
        assert_eq!(file.lang, "erb");
        assert!(store
            .symbols_for_blob("hashErb", lang::ERB.symbol_salt)
            .unwrap()
            .is_empty());
        // "We looked, and there is nothing" is observable on the
        // HIGHLIGHTS side: `put_highlights` writes one row carrying the
        // (empty) span list, so `highlights_for_blob` answers `Some([])`
        // rather than `None`.
        assert_eq!(
            store
                .highlights_for_blob("hashErb", lang::ERB.highlight_salt)
                .unwrap(),
            Some(Vec::new())
        );
        assert_eq!(outcome.highlight_cache, HighlightCache::SkippedTier);
        // V72-H2b — it is now observable on the SYMBOLS side too, and
        // that is the whole fix. `has_symbols` (COUNT > 0) still says
        // "no rows", which is true and useless as a gate; the MARKER says
        // "derived", which is what stops the re-parse.
        assert!(!store.has_symbols("hashErb", lang::ERB.symbol_salt).unwrap());
        assert!(store
            .is_derived("hashErb", lang::SaltFamily::Symbol, lang::ERB.symbol_salt)
            .unwrap());
        assert_eq!(
            store
                .derived_rows("hashErb", lang::SaltFamily::Symbol, lang::ERB.symbol_salt)
                .unwrap(),
            Some(0),
            "Some(0) and None are different answers — that is the table's whole job"
        );
    }

    // ── V72-H2b (D7/D16) — the salt split + the two independent gates ────

    /// The defect V72-H1 reported, as a regression test. A zero-symbol
    /// language visited twice must be a CACHE HIT the second time: before
    /// the marker, `has_symbols`'s `COUNT(*) > 0` made that branch
    /// structurally unreachable and every ERB/SCSS/comment-only file
    /// re-parsed on every single visit, forever.
    #[test]
    fn a_zero_symbol_language_is_a_cache_hit_on_its_second_visit() {
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        for (path, src, hash) in [
            (
                "app/views/orders/show.html.erb",
                &b"<h1><%= @order.id %></h1>\n"[..],
                "hashErb",
            ),
            // Not just ERB: an SCSS file is `highlight_only` (real spans,
            // no symbols), and a comment-only Rust file is `full` tier
            // with an honestly empty symbol set. All three took the same
            // damage.
            ("app/assets/a.scss", &b"$brand: #336699;\n"[..], "hashScss"),
            ("src/notes.rs", &b"// just a comment\n"[..], "hashNotes"),
        ] {
            let first = index_file(&store, repo_id, path, src, hash, true, false, &kw()).unwrap();
            assert!(!first.cache_hit, "{path}: first visit must derive");
            assert_eq!(first.symbol_count, 0, "{path}: fixture must be symbol-less");
            let second = index_file(&store, repo_id, path, src, hash, true, false, &kw()).unwrap();
            assert!(
                second.cache_hit,
                "{path}: a derived blob with zero symbols must be a cache hit"
            );
        }
    }

    /// The HIGHLIGHT gate is independent of the symbol one, in both
    /// directions and by counter.
    #[test]
    fn the_highlight_gate_reports_hit_miss_and_skipped_tier() {
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();

        let first = index_file(
            &store,
            repo_id,
            "src/lib.rs",
            RUST_SRC,
            "hashA",
            true,
            false,
            &kw(),
        )
        .unwrap();
        assert_eq!(first.highlight_cache, HighlightCache::Miss);
        let second = index_file(
            &store,
            repo_id,
            "src/lib.rs",
            RUST_SRC,
            "hashA",
            true,
            false,
            &kw(),
        )
        .unwrap();
        assert_eq!(second.highlight_cache, HighlightCache::Hit);

        // A tier that paints nothing says so on every visit — it is a
        // property of the TYPE, not of the cache.
        let erb = index_file(
            &store,
            repo_id,
            "a.html.erb",
            b"<%= 1 %>\n",
            "hashE",
            true,
            false,
            &kw(),
        )
        .unwrap();
        assert_eq!(erb.highlight_cache, HighlightCache::SkippedTier);
        let erb2 = index_file(
            &store,
            repo_id,
            "a.html.erb",
            b"<%= 1 %>\n",
            "hashE",
            true,
            false,
            &kw(),
        )
        .unwrap();
        assert_eq!(erb2.highlight_cache, HighlightCache::SkippedTier);

        // And a content-capped file never reaches either gate.
        let big = vec![b'a'; (MAX_PARSE_BYTES + 1) as usize];
        let huge = index_file(
            &store,
            repo_id,
            "big.rs",
            &big,
            "hashBig",
            true,
            false,
            &kw(),
        )
        .unwrap();
        assert_eq!(huge.highlight_cache, HighlightCache::SkippedTier);
    }

    /// **Salt independence.** Bumping ONE family's salt re-extracts that
    /// family and leaves the other's rows byte-identical. This is the
    /// property the whole unit exists for, so it is asserted on the rows
    /// themselves rather than on a counter.
    #[test]
    fn bumping_one_familys_salt_leaves_the_others_rows_byte_identical() {
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        index_file(
            &store,
            repo_id,
            "src/lib.rs",
            RUST_SRC,
            "hashA",
            true,
            false,
            &kw(),
        )
        .unwrap();

        let symbols_before = store
            .symbols_for_blob("hashA", lang::RUST.symbol_salt)
            .unwrap();
        let spans_before = store
            .highlights_for_blob("hashA", lang::RUST.highlight_salt)
            .unwrap()
            .expect("painted");
        assert!(!symbols_before.is_empty() && !spans_before.is_empty());

        // Simulate the HIGHLIGHT half of a role-table bump: the new salt
        // has no rows, so the highlight gate misses. The symbol gate must
        // not notice at all.
        let next_hl = "rust@0.24.2+h2+roles3";
        assert!(!store
            .is_derived("hashA", lang::SaltFamily::Highlight, next_hl)
            .unwrap());
        assert!(store
            .is_derived("hashA", lang::SaltFamily::Symbol, lang::RUST.symbol_salt)
            .unwrap());

        // ... and the reverse: a symbol-salt bump leaves the painted rows
        // exactly where they are, addressable under the UNCHANGED
        // highlight salt.
        let next_sym = "rust@0.24.2+q4";
        assert!(!store
            .is_derived("hashA", lang::SaltFamily::Symbol, next_sym)
            .unwrap());
        assert_eq!(
            store
                .highlights_for_blob("hashA", lang::RUST.highlight_salt)
                .unwrap(),
            Some(spans_before.clone()),
            "a symbol-salt bump must not touch a single painted span"
        );

        // Writing the symbol family under the NEW salt purges the old
        // symbol rows (invariant 11) and still leaves highlights alone.
        store
            .replace_symbols("hashA", next_sym, &symbols_before)
            .unwrap();
        assert!(store
            .symbols_for_blob("hashA", lang::RUST.symbol_salt)
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .highlights_for_blob("hashA", lang::RUST.highlight_salt)
                .unwrap(),
            Some(spans_before),
            "the highlight family survived a symbol-family re-derive intact"
        );
    }

    /// The walk's own tally: the three highlight counters sum to `files`,
    /// so a `highlight_salt` bump's cost is readable off the boot log.
    #[test]
    fn walk_stats_highlight_counters_sum_to_the_files_visited() {
        let mut stats = WalkStats::default();
        for c in [
            HighlightCache::Hit,
            HighlightCache::Miss,
            HighlightCache::Miss,
            HighlightCache::SkippedTier,
        ] {
            stats.files += 1;
            stats.record_highlight(c);
        }
        assert_eq!(stats.highlight_hits, 1);
        assert_eq!(stats.highlight_misses, 2);
        assert_eq!(stats.highlight_skipped, 1);
        assert_eq!(
            stats.highlight_hits + stats.highlight_misses + stats.highlight_skipped,
            stats.files
        );
    }

    /// V72-H1 — D7's stem table, through the real pipeline: a `Rakefile`
    /// is Ruby source and now indexes as Ruby, where it used to land in
    /// `TIER_UNKNOWN` (Ruby the instrument silently ignored).
    #[test]
    fn a_stem_table_file_indexes_as_its_real_language() {
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        let src = b"task :seed do\n  puts 1\nend\n";
        let outcome = index_file(
            &store,
            repo_id,
            "Rakefile",
            src,
            "hashRake",
            true,
            false,
            &kw(),
        )
        .unwrap();
        assert_eq!(outcome.tier, "ruby");
        let file = store.get_file(repo_id, "Rakefile").unwrap().unwrap();
        assert_eq!(file.lang, "ruby");
        // And `Gemfile.lock` beside it stays plain, as D7 asks.
        index_file(
            &store,
            repo_id,
            "Gemfile.lock",
            b"GEM\n  remote: https://rubygems.org/\n",
            "hashLock",
            true,
            false,
            &kw(),
        )
        .unwrap();
        let lock = store.get_file(repo_id, "Gemfile.lock").unwrap().unwrap();
        assert_eq!(lock.lang, TIER_UNKNOWN);
    }

    // --- index_repo_working_tree ------------------------------------------

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git -C {} {:?} failed: {}",
            dir.display(),
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A fixture repo covering every ingest tier: a rust file, a python
    /// file, a gitignored file (must never appear), an oversized file, a
    /// binary file, and (unix only) a symlink.
    fn fixture_repo() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["config", "user.email", "test@example.com"]);
        git(dir, &["config", "user.name", "Test"]);

        std::fs::write(dir.join("lib.rs"), RUST_SRC).unwrap();
        std::fs::write(dir.join("app.py"), b"def hi():\n    pass\n").unwrap();
        std::fs::write(dir.join(".gitignore"), b"ignored.rs\n").unwrap();
        std::fs::write(dir.join("ignored.rs"), b"fn ignored() {}\n").unwrap();
        std::fs::write(
            dir.join("big.bin"),
            vec![b'x'; (MAX_PARSE_BYTES + 1) as usize],
        )
        .unwrap();
        std::fs::write(dir.join("photo.bin"), vec![0xff, 0xd8, 0xff, 0xe0]).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("lib.rs", dir.join("a-symlink")).unwrap();

        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "c1"]);
        tmp
    }

    #[test]
    fn walks_a_repo_and_classifies_every_tracked_file() {
        let tmp = fixture_repo();
        let (_state, store) = open_store();
        let repo = GitRepo::open(tmp.path()).unwrap();
        let repo_id = store
            .upsert_repo("fixture", tmp.path().to_str().unwrap())
            .unwrap();

        let stats =
            index_repo_working_tree(&store, &repo, repo_id, "HEAD", true, false, &kw()).unwrap();

        // .gitignore excludes ignored.rs — `git ls-tree` never lists it, so
        // it must never reach the store.
        assert!(store.get_file(repo_id, "ignored.rs").unwrap().is_none());
        // A symlink is walked (kind=Symlink) but deliberately not indexed.
        assert!(store.get_file(repo_id, "a-symlink").unwrap().is_none());

        assert_eq!(
            store.get_file(repo_id, "lib.rs").unwrap().unwrap().lang,
            "rust"
        );
        assert_eq!(
            store.get_file(repo_id, "app.py").unwrap().unwrap().lang,
            "python"
        );
        assert_eq!(
            store.get_file(repo_id, "big.bin").unwrap().unwrap().lang,
            TIER_TOO_LARGE
        );
        assert_eq!(
            store.get_file(repo_id, "photo.bin").unwrap().unwrap().lang,
            TIER_BINARY
        );
        assert_eq!(
            store.get_file(repo_id, ".gitignore").unwrap().unwrap().lang,
            TIER_UNKNOWN
        );

        assert_eq!(stats.files, 5); // lib.rs, app.py, .gitignore, big.bin, photo.bin
        assert_eq!(stats.parsed, 2); // lib.rs, app.py
        assert_eq!(stats.cache_hits, 0);
        assert_eq!(stats.skipped_tier, 3); // .gitignore, big.bin, photo.bin
        assert!(stats.symbols >= 1);
        assert_eq!(store.file_count(repo_id).unwrap(), 5);

        // Re-running the walk on the SAME rev is all cache hits (branch
        // switch that changes nothing re-derives nothing — ADR-2).
        let stats2 =
            index_repo_working_tree(&store, &repo, repo_id, "HEAD", true, false, &kw()).unwrap();
        assert_eq!(stats2.parsed, 0);
        assert_eq!(stats2.cache_hits, 2);
    }
}
