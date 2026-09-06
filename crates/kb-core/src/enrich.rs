//! Post-upsert enrichment hooks (RFC phase X2).
//!
//! After the indexer commits a `Doc` to lance, it runs a sequence of
//! best-effort *enrichers*: session-transcript capture, the memory-recall
//! ledger, memory-link seeding, cross-artifact edge recording, index
//! snapshots, and list anchors. Before X2 the first three were inline `if`
//! blocks in
//! [`crate::indexer`]; this module formalises them as an
//! [`EnrichmentHook`] registry so the *next* sessions-shaped feature is
//! one new `impl` rather than edits scattered across the pipeline.
//!
//! Contract (load-bearing — see the crate's `CLAUDE.md`):
//!
//! - **Best-effort.** A hook's `Err` is logged and skipped; it must never
//!   fail the index. Migrated hooks already self-handle their errors
//!   (logging / `record_failure`) and return `Ok(())`, so the
//!   registry-level log is only a safety net for future hooks.
//! - **Registration order is the run order** — `session-capture` →
//!   `memory-recall-ledger` → `memory-commit-ledger` → `memory-link-seed` →
//!   `edge-record` → `code-refs` → `snapshot-capture` → `list-anchor`.
//!   Order is pinned by `default_hooks_registration_order`.
//! - **Writes go through the [`StorageHandle`]**, never lance/sqlite
//!   directly — the single-writer-per-kb invariant. Hooks are otherwise
//!   read-only on their [`EnrichCtx`].

use crate::events::EventBus;
use crate::ids::{ArtifactId, SourceSlug};
use crate::storage::sqlite::{CodeRefHeaderRow, CodeRefRow};
use crate::storage::StorageHandle;
use crate::types::KbName;
use futures::future::BoxFuture;
use serde_json::json;
use std::path::Path;

/// Everything a hook needs about the just-committed artifact. All borrows —
/// the indexer snapshots the few owned values (`kb_category`, the memory-link
/// seed inputs) *before* `doc` moves into `upsert_doc`, then builds this once
/// and lends it to each hook in turn.
pub struct EnrichCtx<'a> {
    pub kb_name: &'a KbName,
    pub source_slug: &'a SourceSlug,
    pub storage: &'a StorageHandle,
    pub bus: &'a EventBus,
    pub quarantine_dir: &'a Path,
    /// The kb's source root — the directory the indexer walks. The
    /// edge-record hook resolves wikilink targets (`[[…]]`) against the
    /// whole corpus, mapping each candidate's stored-canonical path to its
    /// source-relative form with
    /// [`crate::paths::doc_rel_path_from_canonical`].
    pub source_root: &'a Path,
    /// Absolute path of the source file on disk.
    pub path: &'a Path,
    pub artifact_id: &'a ArtifactId,
    /// Source-relative path (the id's pre-image).
    pub rel_path: &'a str,
    /// The parsed document body (HTML or rendered markdown).
    pub html: &'a str,
    /// Runtime artifact-subdomain suffix (invariant #7) — threaded into
    /// link extraction so production deployments match alongside the dev
    /// `.artifacts.localhost`.
    pub artifact_host_suffix: &'a str,
    /// `kb-category` snapshot — the `interested` pre-filter reads it.
    pub kb_category: Option<&'a str>,
    /// `mtime` (unix secs) snapshotted from the `Doc`. Sessions use it as
    /// `ended_at` and as the transcript parse's clock.
    pub mtime_unix: i64,
    /// DCB W1.A (R4) — wall-clock unix-seconds read ONCE per
    /// `finish_indexed_doc` call and snapshotted here so `CodeRefHook` stays a
    /// pure function of its borrowed `ctx` like every other hook: no hook
    /// reaches for a live clock itself (kb-core's "the caller injects the
    /// clock" shape — `resurface.rs`, `memory::rerank_with_policy`,
    /// `triage::reasons_for`). Distinct from [`Self::mtime_unix`] (the FILE's
    /// mtime): this is "when the indexer ran", not "when the file was last
    /// touched on disk", and unlike mtime it cannot regress below a
    /// consumer's cursor watermark on a checkout/rsync/restore.
    pub now_unix: i64,
    /// Memory-link seed inputs (`kb-global` / `kb-linked-kbs` metas).
    pub seed_global: bool,
    pub seed_linked_kbs: &'a [String],
    /// Track V — raw-bytes content hash (12-hex) of the source file; the
    /// snapshot-capture hook's dedup key. Identical to the value persisted
    /// on the lance `Doc`.
    pub content_hash: &'a str,
    /// Track V — the artifact's raw source text (HTML, or the `.md` for
    /// markdown), stored verbatim by the snapshot-capture hook so the
    /// raw-HTML diff toggle and the re-extracted prose diff both resolve
    /// from one place.
    pub raw_source: &'a str,
    /// Track V — resolved per-kb versions mode. The snapshot hook captures
    /// only when this reads index snapshots (`auto`/`index`/`both`), so a
    /// `git`-only or `off` kb stores nothing.
    pub versions_mode: crate::vcs::VersionsMode,
    /// The R1 digest step's already-computed transcript parse, lent so the
    /// session-capture hook doesn't JSON-parse the same multi-MB transcript
    /// a SECOND time per capture. `Some` exactly when the artifact is a
    /// `memory-session` and `index_file` built the digest; the hook falls
    /// back to parsing `html` itself when absent (non-indexer callers).
    pub session_parse: Option<&'a crate::sessions::SessionParse>,
}

/// A best-effort post-upsert enricher. See the module docs for the contract.
pub trait EnrichmentHook: Send + Sync {
    /// Stable identifier, used only in the registry's failure log.
    fn name(&self) -> &'static str;

    /// Cheap pre-filter — skip the (possibly async) `enrich` when this
    /// artifact isn't relevant to the hook.
    fn interested(&self, ctx: &EnrichCtx<'_>) -> bool;

    /// Run the enrichment. Boxed future to dodge async-fn-in-trait (the same
    /// RPITIT wall the share host hit — see `share/host.rs`). An `Err` is
    /// logged + skipped by the registry; it must never fail the index.
    fn enrich<'a>(&'a self, ctx: &'a EnrichCtx<'a>) -> BoxFuture<'a, Result<(), String>>;
}

/// The default registry, in run order. A `LazyLock<Vec<Box<dyn …>>>` built
/// once (the hooks are zero-sized + stateless) and lent to every index.
pub fn default_hooks() -> &'static [Box<dyn EnrichmentHook>] {
    static HOOKS: std::sync::LazyLock<Vec<Box<dyn EnrichmentHook>>> =
        std::sync::LazyLock::new(|| {
            vec![
                Box::new(SessionCaptureHook) as Box<dyn EnrichmentHook>,
                Box::new(MemoryRecallLedgerHook),
                Box::new(MemoryCommitLedgerHook),
                Box::new(MemoryLinkSeedHook),
                Box::new(EdgeRecordHook),
                Box::new(CodeRefHook),
                Box::new(SnapshotCaptureHook),
                Box::new(ListAnchorHook),
            ]
        });
    &HOOKS
}

/// `memory-session` transcripts are captured into the V0008 `sessions`
/// table; ordinary memory artifacts (`memory-*` minus sessions) are seeded.
fn is_memory_session_category(cat: Option<&str>) -> bool {
    cat == Some("memory-session")
}

/// Any `memory-*` category *except* `memory-session` — verbatim transcripts
/// are deliberately excluded from memory-link seeding (they'd pollute every
/// kb's "Related memories" rail; the operator asked for memories ↔ kbs only).
fn is_memory_artifact_category(cat: Option<&str>) -> bool {
    cat.is_some_and(|c| c.starts_with("memory-") && c != "memory-session")
}

// --- 1. Session capture -----------------------------------------------------

/// v0.14 S2 — parse a `memory-session` transcript into the per-kb `sessions`
/// enrichment table + emit `session.captured`. Pure parse + one sqlite upsert
/// through the storage actor. On failure: log + continue (the lance row
/// carrying `kb_session` is already searchable from the per-memory side).
struct SessionCaptureHook;

impl EnrichmentHook for SessionCaptureHook {
    fn name(&self) -> &'static str {
        "session-capture"
    }

    fn interested(&self, ctx: &EnrichCtx<'_>) -> bool {
        is_memory_session_category(ctx.kb_category)
    }

    fn enrich<'a>(&'a self, ctx: &'a EnrichCtx<'a>) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            // Reuse the indexer's R1 digest parse when it's on the ctx —
            // parsing a multi-MB transcript (extract_pre copy + unescape
            // passes + per-JSONL-line serde) is the dominant cost of a
            // capture, and it's deterministic, so parsing twice bought
            // nothing. Fallback keeps the hook self-sufficient for callers
            // that didn't pre-parse.
            let parsed_here;
            let parse: &crate::sessions::SessionParse = match ctx.session_parse {
                Some(p) => p,
                None => {
                    parsed_here = crate::sessions::parse_session_html_full(
                        ctx.html,
                        ctx.rel_path,
                        ctx.mtime_unix,
                    );
                    &parsed_here
                }
            };
            let facts = &parse.facts;
            let act = &parse.activity;
            let artifact_id = ctx.artifact_id.as_str().to_string();
            // "Correct in place": ended_at = last-event ts, not capture mtime.
            // Fall back to mtime when the transcript carried no timestamp.
            let ended_at = act.ended_at.unwrap_or(ctx.mtime_unix);

            // W0.5 — the envelope's additive subagent digest block
            // (`kb_core::sessions::render_subagents_block`), when present.
            // Sidecar data is SIDECAR-PRIMARY for the aggregate columns below:
            // unlike the parent transcript's own `toolUseResult` (W0.2, which
            // only carries stats for a SYNCHRONOUS completion), the sidecar
            // walk covers async/background delegations too, since it reads
            // each subagent's own transcript directly. Absent block (old
            // captures, a session with no delegations, or one whose sidecars
            // were pruned before capture) ⇒ unchanged W0.2 parent-only
            // behavior.
            let subagents_block = crate::sessions::extract_subagents_block(ctx.html);
            let (
                subagent_count,
                subagent_tokens,
                subagent_tool_calls,
                subagent_files_edited,
                subagent_launched_unstatted,
            ): (u32, u64, u32, u32, u32) = match &subagents_block {
                Some(block) => {
                    let count = block.agents.len() as u32;
                    let tokens: u64 = block.agents.iter().map(|a| a.tokens).sum();
                    let tool_calls: u32 = block
                        .agents
                        .iter()
                        .map(|a| a.tool_calls)
                        .fold(0u32, u32::saturating_add);
                    // No distinct-file snapshot per subagent (unlike the main
                    // session's `edited_paths`), so this stays an edit-
                    // OPERATION count — same "detected, not ground truth"
                    // (#10) caveat as the W0.2 parent-only fallback.
                    let files_edited: u32 = block
                        .agents
                        .iter()
                        .map(|a| a.files.iter().filter(|f| f.action != "read").count() as u32)
                        .fold(0u32, u32::saturating_add);
                    // "Launched, per the PARENT transcript" (both the
                    // stat-bearing AND the stub delegations W0.2 already
                    // detects) MINUS however many the sidecar walk actually
                    // matched (one sidecar file parsed = one match), floored
                    // at 0: a launch the sidecar walk covered is no longer
                    // "unstatted" — its real numbers now live in `block`.
                    let launched_total = act.subagent_count + act.subagent_launched_unstatted;
                    let unstatted = launched_total.saturating_sub(count);
                    (count, tokens, tool_calls, files_edited, unstatted)
                }
                None => (
                    act.subagent_count,
                    act.subagent_tokens,
                    act.subagent_tool_calls,
                    act.subagent_files_edited,
                    act.subagent_launched_unstatted,
                ),
            };

            // V0029/P2 — the envelope's additive `<script id="kb-session-
            // commits">` block, hoisted here (BEFORE the row build) so it
            // serves double duty: `derive_project`'s rung-2 ladder below AND
            // the `session_commits` persistence pass further down reuse the
            // SAME parse instead of re-scanning the tail block twice.
            let captured_commits_block = crate::sessions::extract_commits_block(ctx.html);

            // P1/V0029 — project_key/repo_root: rung 2 (modal repo_root over
            // RESOLVED commit rows) else rung 3 (modal cwd). See
            // `derive_project`'s doc comment for why rung 1 (a forward-
            // capture tail block) is out of this wave's scope.
            let (project_key, repo_root) = crate::sessions::derive_project(
                act.cwd.as_deref(),
                captured_commits_block.as_deref().unwrap_or(&[]),
            );

            // R5/V0029 — harness ladder rungs 2+3: rung 1 (adapter-meta) is
            // already on `act.harness` (parse_session_activity walks the
            // JSONL); rung 2 is the envelope-head `<meta name="kb-harness">`
            // (only the raw HTML head has it); rung 3 is the closed-set
            // default.
            let harness = act
                .harness
                .clone()
                .or_else(|| crate::sessions::meta_harness(ctx.html))
                .unwrap_or_else(|| crate::sessions::HARNESS_DEFAULT.to_string());

            // P1/V0029 — JSON array of every distinct cwd, NULL when the
            // session only ever touched one (the plain `cwd` column already
            // covers that case).
            let all_cwds = (act.all_cwds.len() > 1)
                .then(|| serde_json::to_string(&act.all_cwds).unwrap_or_default());

            // S1/V0029 — the deterministic substance triage ladder. Commit
            // count is computed here from the SAME `captured_commits_block`
            // the persistence pass below writes (kind=="commit" only —
            // push/tag excluded, matching the badge's meaning), falling back
            // to the transcript-only `act.commits` when there's no tail
            // block (mirrors the persistence duality below).
            let commit_count: u32 = match &captured_commits_block {
                Some(captured) => captured.iter().filter(|c| c.kind == "commit").count() as u32,
                None => act.commits.iter().filter(|c| c.kind == "commit").count() as u32,
            };
            let substance = crate::sessions::session_substance(
                act.last_assistant_text.is_some(),
                act.tool_calls,
                act.files_edited_count(),
                commit_count,
                act.user_turns,
            );

            let row = crate::storage::sqlite::SessionRow {
                artifact_id: artifact_id.clone(),
                session_id: facts.session_id.clone(),
                started_at: facts.started_at,
                ended_at,
                message_count: facts.message_count,
                first_user_prompt: facts.first_user_prompt.clone(),
                source_relative: ctx.rel_path.to_string(),
                title: act.ai_title.clone(),
                cwd: act.cwd.clone(),
                git_branch: act.git_branch.clone(),
                files_read_count: act.files_read_count(),
                files_edited_count: act.files_edited_count(),
                token_total: act.token_total,
                tool_calls: act.tool_calls,
                model: act.model.clone(),
                error_count: act.error_count,
                subagent_count,
                subagent_tokens,
                subagent_tool_calls,
                subagent_files_edited,
                subagent_launched_unstatted,
                project_key,
                repo_root,
                harness,
                cc_version: act.cc_version.clone(),
                last_assistant_text: act.last_assistant_text.clone(),
                all_cwds,
                commit_count,
                user_turns: act.user_turns,
                active_secs: act.active_secs,
                substance: Some(substance.to_string()),
            };
            if let Err(e) = ctx.storage.sessions_upsert(row).await {
                tracing::warn!(
                    kb = %ctx.kb_name,
                    path = %ctx.path.display(),
                    session_id = %facts.session_id,
                    error = %e,
                    "sessions_upsert failed",
                );
                return Ok(());
            }

            // V0017 / S3 — persist the session->file edges, resolving each
            // touched path against the daemon's corpus mount table so an
            // in-corpus file links to its artifact (A4/A7); out-of-corpus
            // paths persist with in_corpus=0 and a plain path (A6). The mount
            // table is daemon-global config (empty in tests → all out-of-corpus).
            let mounts = crate::sessions::corpus_mounts();
            let cwd = act.cwd.as_deref();
            let mut files: Vec<crate::storage::sqlite::SessionFileRow> = act
                .files
                .iter()
                .map(|f| {
                    let basename = f.path.rsplit('/').next().unwrap_or(&f.path).to_string();
                    let (in_corpus, target_kb, target_artifact_id) =
                        crate::sessions::resolve_corpus_path(&f.path, cwd, &mounts);
                    crate::storage::sqlite::SessionFileRow {
                        artifact_id_session: artifact_id.clone(),
                        session_id: facts.session_id.clone(),
                        path: f.path.clone(),
                        basename,
                        action: f.action.as_str().to_string(),
                        in_corpus,
                        target_kb,
                        target_artifact_id,
                        via_subagent: false,
                    }
                })
                .collect();
            // W0.5 — fold in the subagent digest block's file touches,
            // deduped against the main thread on (path, action) with the
            // MAIN-THREAD row winning a collision: a file the main thread
            // also touched is authoritative (and already in `files` above);
            // the subagent's copy of the same touch is redundant, not
            // contradictory, so it's simply dropped rather than overwriting.
            if let Some(block) = &subagents_block {
                let mut seen: std::collections::HashSet<(String, String)> = files
                    .iter()
                    .map(|f| (f.path.clone(), f.action.clone()))
                    .collect();
                for agent in &block.agents {
                    for f in &agent.files {
                        let key = (f.path.clone(), f.action.clone());
                        if !seen.insert(key) {
                            continue;
                        }
                        let basename = f.path.rsplit('/').next().unwrap_or(&f.path).to_string();
                        let (in_corpus, target_kb, target_artifact_id) =
                            crate::sessions::resolve_corpus_path(&f.path, cwd, &mounts);
                        files.push(crate::storage::sqlite::SessionFileRow {
                            artifact_id_session: artifact_id.clone(),
                            session_id: facts.session_id.clone(),
                            path: f.path.clone(),
                            basename,
                            action: f.action.clone(),
                            in_corpus,
                            target_kb,
                            target_artifact_id,
                            via_subagent: true,
                        });
                    }
                }
            }
            if let Err(e) = ctx
                .storage
                .session_files_replace(artifact_id.clone(), files)
                .await
            {
                tracing::warn!(
                    kb = %ctx.kb_name,
                    session_id = %facts.session_id,
                    error = %e,
                    "session_files_replace failed",
                );
            }

            // S9 — persist the decisions log (steering moments).
            let decisions: Vec<crate::storage::sqlite::SessionDecisionRow> = act
                .decisions
                .iter()
                .enumerate()
                .map(|(i, d)| crate::storage::sqlite::SessionDecisionRow {
                    artifact_id_session: artifact_id.clone(),
                    session_id: facts.session_id.clone(),
                    seq: i as i64,
                    kind: d.kind.clone(),
                    prompt: d.prompt.clone(),
                    answer: d.answer.clone(),
                })
                .collect();
            if let Err(e) = ctx
                .storage
                .session_decisions_replace(artifact_id.clone(), decisions)
                .await
            {
                tracing::warn!(
                    kb = %ctx.kb_name,
                    session_id = %facts.session_id,
                    error = %e,
                    "session_decisions_replace failed",
                );
            }

            // P5/W0.4 — persist the commits the session produced. Prefer the
            // envelope's additive `<script id="kb-session-commits">` block
            // (capture-time git resolution — `kb sessions capture` /
            // `vcs::resolve_commit`) when present; fall back to
            // transcript-only detection (`resolved: false` on every row) for
            // old captures, `kb import claude-history` backfills, and any
            // hookless writer that never resolved commits. Reuses the SAME
            // parse `captured_commits_block` above already produced.
            let commits: Vec<crate::storage::sqlite::SessionCommitRow> =
                match captured_commits_block {
                    Some(captured) => captured
                        .into_iter()
                        .enumerate()
                        .map(|(i, c)| crate::storage::sqlite::SessionCommitRow {
                            artifact_id_session: artifact_id.clone(),
                            session_id: facts.session_id.clone(),
                            seq: i as i64,
                            kind: c.kind,
                            sha: c.sha,
                            subject: c.subject,
                            sha_full: c.sha_full,
                            repo_root: c.repo_root,
                            resolved: c.resolved,
                            author: c.author,
                            parents: c.parents.map(i64::from),
                            trailers: (!c.trailers.is_empty()).then(|| c.trailers.join("\n")),
                        })
                        .collect(),
                    None => act
                        .commits
                        .iter()
                        .enumerate()
                        .map(|(i, c)| crate::storage::sqlite::SessionCommitRow {
                            artifact_id_session: artifact_id.clone(),
                            session_id: facts.session_id.clone(),
                            seq: i as i64,
                            kind: c.kind.clone(),
                            sha: c.sha.clone(),
                            subject: c.subject.clone(),
                            ..Default::default()
                        })
                        .collect(),
                };
            if let Err(e) = ctx
                .storage
                .session_commits_replace(artifact_id.clone(), commits)
                .await
            {
                tracing::warn!(
                    kb = %ctx.kb_name,
                    session_id = %facts.session_id,
                    error = %e,
                    "session_commits_replace failed",
                );
            }

            // R4 — persist the research / tool-usage signals the session
            // produced (kb/web searches, subagents, skills, plan spans).
            let research: Vec<crate::storage::sqlite::SessionResearchRow> = act
                .research
                .iter()
                .enumerate()
                .map(|(i, r)| crate::storage::sqlite::SessionResearchRow {
                    artifact_id_session: artifact_id.clone(),
                    session_id: facts.session_id.clone(),
                    seq: i as i64,
                    kind: r.kind.clone(),
                    query: r.query.clone(),
                })
                .collect();
            if let Err(e) = ctx
                .storage
                .session_research_replace(artifact_id.clone(), research)
                .await
            {
                tracing::warn!(
                    kb = %ctx.kb_name,
                    session_id = %facts.session_id,
                    error = %e,
                    "session_research_replace failed",
                );
            }

            ctx.bus.emit(
                "session.captured",
                json!({
                    "kb": ctx.kb_name.as_str(),
                    "artifact_id": ctx.artifact_id.as_str(),
                    "session_id": &facts.session_id,
                    "started_at": facts.started_at,
                    "message_count": facts.message_count,
                }),
            );
            Ok(())
        })
    }
}

// --- 1b. Memory-recall ledger (MI-W1.1) -------------------------------------

/// MI-W1.1 (revised) — derive the `memory_recalls` ledger (V0035) from the
/// just-captured transcript's `Item::MemoryInjection` items (session-view/1
/// IR) and replace THIS CAPTURE's row set wholesale (scoped to
/// `ctx.artifact_id`, not the session id — see `memory_recalls_replace`'s
/// doc comment; reads scope to the newest capture instead). Runs AFTER
/// `SessionCaptureHook` (same `interested` gate) — a sibling, not a merge,
/// because it needs the Turn/Item IR (`session_view_for_capture_html`),
/// which `SessionCaptureHook` deliberately does NOT build (it only needs the
/// cheaper facts/activity parse). Best-effort: any storage error logs +
/// continues, never fails the index. **CT-A3**: also reads
/// `derive_memory_recalls`'s three-way parse census and warns (greppable,
/// ndjson-logged) when an injection happened this capture but the parse
/// dropped some or all of its hits — the `kb-recall/1` marker grammar this
/// guards against silently going stale lives in
/// `crate::sessions::view::RECALL_MARKER_PREFIX`.
struct MemoryRecallLedgerHook;

impl EnrichmentHook for MemoryRecallLedgerHook {
    fn name(&self) -> &'static str {
        "memory-recall-ledger"
    }

    fn interested(&self, ctx: &EnrichCtx<'_>) -> bool {
        is_memory_session_category(ctx.kb_category)
    }

    fn enrich<'a>(&'a self, ctx: &'a EnrichCtx<'a>) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            // The session id — reuse the R1 digest parse when it's on the
            // ctx (same fallback SessionCaptureHook uses); this hook still
            // pays for its OWN Turn/Item IR parse below, since that
            // interpretation isn't shared anywhere yet (a known perf
            // hazard for very large transcripts — see the module docs).
            let parsed_here;
            let facts_session_id: &str = match ctx.session_parse {
                Some(p) => &p.facts.session_id,
                None => {
                    parsed_here = crate::sessions::parse_session_html_full(
                        ctx.html,
                        ctx.rel_path,
                        ctx.mtime_unix,
                    );
                    &parsed_here.facts.session_id
                }
            };
            let session_id = facts_session_id.to_string();
            let view = crate::sessions::view::session_view_for_capture_html(ctx.html);
            let derived = crate::sessions::view::derive_memory_recalls(&view);
            // CT-A3 — the three-way parse census (`kb-recall/1` marker vs.
            // the free-text fallback vs. an injected hit that parsed via
            // NEITHER), read off `derived` before its `rows` are consumed
            // below. Exposed here (rather than only inline) so a future
            // consumer (e.g. `kb doctor`) can eventually surface the same
            // counts without re-deriving.
            let injections = derived.marker_parsed + derived.fallback_parsed + derived.failed;
            let marker_parsed = derived.marker_parsed;
            let fallback_parsed = derived.fallback_parsed;
            let failed = derived.failed;
            let artifact_id = ctx.artifact_id.as_str().to_string();
            let rows: Vec<crate::storage::sqlite::MemoryRecallRow> = derived
                .rows
                .into_iter()
                .map(|d| crate::storage::sqlite::MemoryRecallRow {
                    memory_kb: d.memory_kb,
                    memory_id: d.memory_id,
                    session_id: session_id.clone(),
                    turn_id: Some(d.turn_id),
                    recalled_at: d.recalled_at,
                    artifact_id: artifact_id.clone(),
                    used: d.used,
                    // MR1 — straight through from the marker; never
                    // synthesised from the hit's index (layout v2-last
                    // prints the pack reversed).
                    pos: d.pos,
                })
                .collect();
            let count = rows.len();
            // An injection happened this capture but the parse produced no
            // row for it (or only SOME of the injected hits made it) —
            // greppable (ndjson-logged), so a `kb-recall.sh` reformat that
            // breaks the fallback grammar is never a silent zero in the
            // `memory_recalls` ledger.
            if injections > 0 && (count == 0 || failed > 0) {
                tracing::warn!(
                    kb = %ctx.kb_name,
                    path = %ctx.path.display(),
                    session_id = %session_id,
                    artifact_id = %artifact_id,
                    injections,
                    marker_parsed,
                    fallback_parsed,
                    failed,
                    "memory recall injection parse gap: some injected hits produced no memory_recalls row",
                );
            }
            if let Err(e) = ctx
                .storage
                .memory_recalls_replace(artifact_id.clone(), rows)
                .await
            {
                tracing::warn!(
                    kb = %ctx.kb_name,
                    path = %ctx.path.display(),
                    session_id = %session_id,
                    artifact_id = %artifact_id,
                    error = %e,
                    "memory_recalls_replace failed",
                );
                return Ok(());
            }
            // CT-F5 — persist the same three counters (V0039's nullable
            // `sessions` columns). Until now the census was ndjson-LOGGED
            // only, which is greppable but not queryable: nothing could
            // answer "what share of injected hits parse via neither grammar,
            // corpus-wide" without replaying logs, so the ledger
            // marker-parse-failure SLO was unmeasurable. This is the ONE new
            // write CT-F5 adds.
            //
            // Written AFTER `memory_recalls_replace` succeeds, so the census
            // and the ledger it describes always agree — a census claiming
            // hits whose rows failed to land would be worse than no census.
            // The UPDATE targets the `sessions` row `SessionCaptureHook`
            // wrote a moment ago (registration order, invariant kb-core #5);
            // a miss (0 rows) leaves the census NULL, which the indicator
            // reports as `unknown` rather than as a zero.
            if let Err(e) = ctx
                .storage
                .sessions_set_recall_census(
                    artifact_id.clone(),
                    marker_parsed as u32,
                    fallback_parsed as u32,
                    failed as u32,
                )
                .await
            {
                // Non-fatal, and deliberately NOT an early return: the ledger
                // rows — the load-bearing product of this hook — are already
                // committed. A missing census only costs one SLO indicator.
                tracing::warn!(
                    kb = %ctx.kb_name,
                    path = %ctx.path.display(),
                    session_id = %session_id,
                    artifact_id = %artifact_id,
                    error = %e,
                    "sessions_set_recall_census failed (CT-F5 census skipped for this capture)",
                );
            }
            // One event per CAPTURE with the count, never one per row
            // (event-storm avoidance — a busy session can recall dozens of
            // memories across its turns).
            ctx.bus.emit(
                "session.memory_recalls",
                json!({
                    "kb": ctx.kb_name.as_str(),
                    "artifact_id": ctx.artifact_id.as_str(),
                    "session_id": &session_id,
                    "count": count,
                }),
            );
            Ok(())
        })
    }
}

// --- 1c. Memory↔commit ledger (CT-F1) ---------------------------------------

/// CT-F1 — the memory↔commit EXACT-ID join. ONE parse arm over data the
/// capture already had: the envelope's additive `<script
/// id="kb-session-commits">` block (`sessions::extract_commits_block`)
/// carries each commit's capture-time `git show` resolution INCLUDING its
/// verbatim trailer lines, which `session_commits.trailers` has persisted —
/// and never read back — since V0025. This hook reads the `Kb-Memory:`
/// lines out of them (`sessions::memory_ids_from_trailers`, pure + closed
/// grammar) and writes `memory_commits` rows (V0038).
///
/// A SIBLING of `MemoryRecallLedgerHook`, not a merge into
/// `SessionCaptureHook`, for the same reason that one is: same `interested`
/// gate, its own derivation, its own table, its own failure mode. It is
/// deliberately CHEAPER than either — it needs no transcript IR and no
/// full facts parse when the tail block is present, only the tail block
/// itself, so a capture with no commits costs one `str::find`.
///
/// **No git call, ever.** `sha_full`/`subject`/`repo_root` are copied from
/// the block the capture already resolved; a commit that never resolved
/// (no `sha_full`) or isn't a `kind == "commit"` row is skipped rather than
/// looked up — inventing a second git read is exactly what this design
/// refuses (the trailers only exist on a resolved row anyway).
///
/// The transcript-only fallback `SessionCaptureHook` uses for commits has
/// no analogue here BY CONSTRUCTION: trailers come from git resolution, so
/// an unresolved capture has none to parse.
struct MemoryCommitLedgerHook;

impl EnrichmentHook for MemoryCommitLedgerHook {
    fn name(&self) -> &'static str {
        "memory-commit-ledger"
    }

    fn interested(&self, ctx: &EnrichCtx<'_>) -> bool {
        is_memory_session_category(ctx.kb_category)
    }

    fn enrich<'a>(&'a self, ctx: &'a EnrichCtx<'a>) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let Some(captured) = crate::sessions::extract_commits_block(ctx.html) else {
                // No tail block at all (an old capture, an import backfill, a
                // commit-less session). Nothing to parse and — critically —
                // nothing to CLEAR either: a capture that can't see trailers
                // must not delete claims an earlier one recorded from git.
                return Ok(());
            };
            let artifact_id = ctx.artifact_id.as_str().to_string();
            let mut rows: Vec<crate::storage::sqlite::MemoryCommitRow> = Vec::new();
            let mut malformed = 0usize;
            let mut ids_seen = 0usize;
            for c in &captured {
                let parse = crate::sessions::memory_ids_from_trailers(&c.trailers);
                malformed += parse.malformed;
                ids_seen += parse.ids.len();
                // Only a RESOLVED commit has a full sha to key on. A push/tag
                // row, or a detection git couldn't resolve, is dropped: the
                // whole point of this table is an EXACT id→commit join, and a
                // short/absent sha is not exact.
                let (Some(sha_full), true) = (c.sha_full.as_deref(), c.kind == "commit") else {
                    continue;
                };
                for memory_id in parse.ids {
                    rows.push(crate::storage::sqlite::MemoryCommitRow {
                        memory_id,
                        sha_full: sha_full.to_string(),
                        sha: c.sha.clone(),
                        subject: c.subject.clone(),
                        repo_root: c.repo_root.clone(),
                        session_id: String::new(), // filled below
                        artifact_id: artifact_id.clone(),
                        // The indexer's injected clock — "when this row was
                        // derived", never a commit timestamp (the envelope
                        // carries none and we refuse a second git read).
                        recorded_at: ctx.now_unix,
                    });
                }
            }
            // A `Kb-Memory:` line that didn't parse is greppable, exactly like
            // the recall hook's injection-parse gap: the trailer grammar lives
            // in a shell hook this crate can't type-check, so drift must be
            // loud rather than a silent zero.
            if malformed > 0 {
                tracing::warn!(
                    kb = %ctx.kb_name,
                    path = %ctx.path.display(),
                    artifact_id = %artifact_id,
                    malformed,
                    parsed = ids_seen,
                    "Kb-Memory trailer parse gap: some trailers were not a bare 12-hex memory id",
                );
            }
            if rows.is_empty() {
                // The overwhelmingly common case — the committing repo never
                // opted into `kb.memoryTrailers` (default OFF). Skip the write
                // entirely rather than run a delete-only transaction on every
                // single capture; a capture that ONCE had rows and now has
                // none is the vanishingly rare inverse, and leaving its old
                // claims is the honest outcome (git history still carries the
                // trailer — see `memory_commits_replace`'s doc comment).
                return Ok(());
            }
            // The session id is only needed once rows exist, so it's resolved
            // HERE — after the early return above — keeping the no-trailer
            // path free of the facts parse entirely.
            let parsed_here;
            let session_id: &str = match ctx.session_parse {
                Some(p) => &p.facts.session_id,
                None => {
                    parsed_here = crate::sessions::parse_session_html_full(
                        ctx.html,
                        ctx.rel_path,
                        ctx.mtime_unix,
                    );
                    &parsed_here.facts.session_id
                }
            };
            for r in &mut rows {
                r.session_id = session_id.to_string();
            }
            let count = rows.len();
            if let Err(e) = ctx
                .storage
                .memory_commits_replace(artifact_id.clone(), rows)
                .await
            {
                tracing::warn!(
                    kb = %ctx.kb_name,
                    path = %ctx.path.display(),
                    session_id = %session_id,
                    artifact_id = %artifact_id,
                    error = %e,
                    "memory_commits_replace failed",
                );
                return Ok(());
            }
            // One event per CAPTURE with the count (the recall ledger's
            // event-storm rule — never one per row).
            ctx.bus.emit(
                "session.memory_commits",
                json!({
                    "kb": ctx.kb_name.as_str(),
                    "artifact_id": ctx.artifact_id.as_str(),
                    "session_id": session_id,
                    "count": count,
                }),
            );
            Ok(())
        })
    }
}

// --- 2. Memory-link seeding -------------------------------------------------

/// L3 — first-time memory-link seeding from HTML metas. The V0011 tombstone
/// is the source-of-truth for "already seeded"; once marked we never
/// re-import from metas, so a user who clears every link in the UI doesn't
/// get them rewritten when the file is touched.
struct MemoryLinkSeedHook;

impl EnrichmentHook for MemoryLinkSeedHook {
    fn name(&self) -> &'static str {
        "memory-link-seed"
    }

    fn interested(&self, ctx: &EnrichCtx<'_>) -> bool {
        is_memory_artifact_category(ctx.kb_category)
    }

    fn enrich<'a>(&'a self, ctx: &'a EnrichCtx<'a>) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let id = ctx.artifact_id.as_str().to_string();
            match ctx.storage.memory_links_seeded_has(id.clone()).await {
                Ok(true) => {} // already seeded; UI is source of truth.
                Ok(false) => {
                    if ctx.seed_global || !ctx.seed_linked_kbs.is_empty() {
                        if let Err(e) = ctx
                            .storage
                            .memory_links_replace(
                                id.clone(),
                                ctx.seed_linked_kbs.to_vec(),
                                ctx.seed_global,
                                crate::indexer::unix_now(),
                            )
                            .await
                        {
                            tracing::warn!(
                                kb = %ctx.kb_name,
                                artifact_id = %id,
                                error = %e,
                                "memory_links_replace (seed) failed",
                            );
                        }
                    }
                    if let Err(e) = ctx
                        .storage
                        .memory_links_seeded_mark(id.clone(), crate::indexer::unix_now())
                        .await
                    {
                        tracing::warn!(
                            kb = %ctx.kb_name,
                            artifact_id = %id,
                            error = %e,
                            "memory_links_seeded_mark failed",
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        kb = %ctx.kb_name,
                        artifact_id = %id,
                        error = %e,
                        "memory_links_seeded_has failed",
                    );
                }
            }
            Ok(())
        })
    }
}

// --- 3. Cross-artifact edge recording ---------------------------------------

/// v0.3 F1 — record outbound edges so the cross-artifact graph route has
/// data. Runs for every artifact. Two passes (id-bearing links, then
/// relative hrefs resolved against the source dir), deduped on target id.
/// An edge-write failure is surfaced to the Errors tab via `record_failure`
/// (the doc upsert already succeeded, so we keep going).
struct EdgeRecordHook;

impl EnrichmentHook for EdgeRecordHook {
    fn name(&self) -> &'static str {
        "edge-record"
    }

    fn interested(&self, _ctx: &EnrichCtx<'_>) -> bool {
        true
    }

    fn enrich<'a>(&'a self, ctx: &'a EnrichCtx<'a>) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let mut outbound: Vec<(String, String)> = Vec::new();
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            // One parse of the document's anchors for BOTH the artifact-id
            // links and the relative-path hrefs.
            let (artifact_links, relative_hrefs) =
                crate::parser::extract_links_and_hrefs(ctx.html, ctx.artifact_host_suffix);
            for to in artifact_links {
                if seen.insert(to.clone()) {
                    outbound.push((to, "link".to_string()));
                }
            }
            let source_dir = ctx.path.parent().map(Path::to_path_buf);
            if let Some(base_dir) = source_dir {
                // Canonicalize the artifact's own path ONCE here, not per href.
                let self_canon = tokio::fs::canonicalize(ctx.path).await.ok();
                // Two phases so a link-heavy hub page costs ONE storage
                // round-trip, not one per href: canonicalise + filter every
                // candidate first (href order preserved — it decides edge
                // order below), then resolve the whole set with a single
                // `path IN (…)` lookup keyed back by stored path.
                let mut candidates: Vec<String> = Vec::new();
                let mut candidate_set: std::collections::HashSet<String> =
                    std::collections::HashSet::new();
                for href in relative_hrefs {
                    // Strip query + fragment before filesystem resolution.
                    let path_part = href.split(['?', '#']).next().unwrap_or(&href).trim();
                    if path_part.is_empty() {
                        continue;
                    }
                    // Root-anchored paths (`/foo`) are left alone — they can't
                    // refer to a kb-source file, and probing them would leak
                    // indexer state about the host filesystem.
                    if path_part.starts_with('/') {
                        continue;
                    }
                    let candidate = base_dir.join(path_part);
                    let Ok(canon) = tokio::fs::canonicalize(&candidate).await else {
                        continue;
                    };
                    // Self-edges are noise; skip when resolved == own file.
                    if self_canon.as_ref() == Some(&canon) {
                        continue;
                    }
                    let canon_str = canon.to_string_lossy().to_string();
                    if candidate_set.insert(canon_str.clone()) {
                        candidates.push(canon_str);
                    }
                }
                if !candidates.is_empty() {
                    match ctx.storage.get_by_source_paths(candidates.clone()).await {
                        Ok(rows) => {
                            let by_path: std::collections::HashMap<&str, &str> = rows
                                .iter()
                                .map(|d| (d.path.as_str(), d.id.as_str()))
                                .collect();
                            for canon_str in &candidates {
                                let Some(&id) = by_path.get(canon_str.as_str()) else {
                                    continue;
                                };
                                if id != ctx.artifact_id.as_str() && seen.insert(id.to_string()) {
                                    outbound.push((id.to_string(), "link".to_string()));
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                kb = %ctx.kb_name,
                                path = %ctx.path.display(),
                                hrefs = candidates.len(),
                                error = %e,
                                "cross-artifact link lookup failed"
                            );
                        }
                    }
                }
            }
            // Wikilinks (`[[target]]`) in Markdown sources — notes especially.
            // The render path leaves `[[…]]` literal (resolution needs storage,
            // render_fragment is pure), so the rendered HTML carries no wikilink
            // anchors; parse them from the raw `.md` and resolve each target
            // against the corpus. Resolved targets join the SAME `kind="link"`
            // edge set, so a note's references become first-class citizens of
            // the backlink/outlink counts, the atlas link-layer, and the graph
            // route — the connective tissue the corpus previously only had for
            // HTML `<a href>` links. Cheap-gated on a literal `[[` so the
            // corpus-wide candidate fetch only runs for docs that link.
            //
            // RE-INVESTIGATED for memory artifacts (2026-08 close-out, Unit 4)
            // and DECLINED AGAIN, with a sharper reason than "memories are
            // HTML": widening this gate to admit memory HTML would not just
            // fail to help, it would silently do NOTHING. `parse_wikilinks`
            // (`crate::links`, comrak's CommonMark block parser) never finds
            // a wikilink inside ANY `<p>...</p>`-wrapped content — proven by
            // `links::tests::
            // wikilinks_are_invisible_inside_any_p_wrapped_html_only_bare_text_works`,
            // which round-trips a full memory HTML document, a single `<p>`
            // fragment, and multiple blank-line-separated `<p>` blocks and
            // gets `[]` every time; only genuinely tag-free plain text
            // triggers the extension. `crate::memory::text_to_body_html`
            // wraps EVERY memory body in `<p>…</p>` unconditionally, so this
            // isn't an edge case — it's the universal case. Fixing it for
            // real would mean feeding `parse_wikilinks` a de-HTML'd plain-
            // text reconstruction of the body instead of the stored source,
            // which (a) has no general inverse for a memory body that ISN'T
            // `text_to_body_html`'s own output (`kb remember --source
            // fetched-web`, SPA-authored bodies, anything with real markup)
            // and (b) stands up a SECOND html→text extraction/sanitization
            // pipeline whose entity-unescaping and tag-stripping edge cases
            // would have to independently agree with what the corpus
            // actually renders — the exact "two parsers drift apart" risk
            // invariant #29 already manages for the Rust/comrak ↔ SPA/hast
            // pair, now with a third. That's a rework of the note-only
            // assumption, not a safe small widening — declined until there's
            // an actual plan for that inverse-extraction step.
            if crate::indexer::is_markdown(ctx.path) && ctx.raw_source.contains("[[") {
                let links = crate::links::parse_wikilinks(ctx.raw_source);
                if !links.is_empty() {
                    match ctx.storage.list_docs(u32::MAX).await {
                        Ok(rows) => {
                            // Stored `Doc.path` is already canonical
                            // (invariant #27), so canonicalise the root ONCE
                            // and strip per row — the old per-row
                            // `doc_rel_path` re-canonicalised BOTH sides,
                            // 2×corpus syscalls per linked note.
                            let canon_root = crate::paths::canonical_abs(ctx.source_root);
                            let candidates: Vec<crate::links::DocLite> = rows
                                .iter()
                                .map(|d| crate::links::DocLite {
                                    id: d.id.clone(),
                                    rel_path: crate::paths::doc_rel_path_from_canonical(
                                        &d.path,
                                        &canon_root,
                                    ),
                                    title: d.title.clone(),
                                })
                                .collect();
                            // One ResolveIndex per note: the lowercased
                            // title/basename keys are built once, then every
                            // target is hashmap work instead of an O(corpus)
                            // re-lowercasing scan.
                            let index = crate::links::ResolveIndex::new(&candidates);
                            let mut targets_seen: std::collections::HashSet<String> =
                                std::collections::HashSet::new();
                            for link in &links {
                                let t = crate::links::normalize_target(&link.target);
                                if !targets_seen.insert(t.clone()) {
                                    continue;
                                }
                                if let crate::links::Resolution::One(to) = index.resolve(&t) {
                                    if to != ctx.artifact_id.as_str() && seen.insert(to.clone()) {
                                        outbound.push((to, "link".to_string()));
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                kb = %ctx.kb_name,
                                path = %ctx.path.display(),
                                error = %e,
                                "list_docs for wikilink resolution failed"
                            );
                        }
                    }
                }
            }
            if let Err(e) = ctx
                .storage
                .record_edges(ctx.artifact_id.as_str().to_string(), outbound)
                .await
            {
                tracing::warn!(kb = %ctx.kb_name, path = %ctx.path.display(), error = %e, "record_edges failed");
                // v0.7.1 P4 — surface to the UI/TUI/error list. `content_hash
                // = None` keeps it out of the per-(path,hash) retry counter
                // (the artifact itself indexed fine; the edge write is a
                // separable follow-on); the next successful reindex's
                // auto-dismiss clears this hashless row. `record_failure`
                // returns Err, but the doc upsert already succeeded — keep
                // going (don't propagate to the index).
                let _ = crate::indexer::record_failure(
                    ctx.kb_name,
                    ctx.source_slug,
                    ctx.storage,
                    ctx.bus,
                    ctx.quarantine_dir,
                    ctx.path,
                    "storage",
                    format!("record_edges: {e}"),
                    None,
                )
                .await;
            }
            Ok(())
        })
    }
}

// --- 3b. Code-reference extraction (DCB W1.A) -------------------------------

/// Flatten an [`Extraction`] into storage rows. Lives HERE, not in
/// `coderefs.rs` — the pure module must not know about storage row types.
/// `group: Option<usize>` becomes the denormalised
/// `(group_key, group_label, group_anchor)` triple (`None` ⇒ all three NULL,
/// the R9 "no sentinel group" rule), and `context_tokens` is space-joined.
fn to_code_ref_rows(extraction: &crate::coderefs::Extraction) -> Vec<CodeRefRow> {
    extraction
        .refs
        .iter()
        .map(|r| {
            let g = r.group.and_then(|i| extraction.groups.get(i));
            CodeRefRow {
                ordinal: r.ordinal,
                kind: r.kind.as_str().to_string(),
                raw_text: r.raw_text.clone(),
                path_hint: r.path_hint.clone(),
                line_start: r.line_start,
                line_end: r.line_end,
                line_spans: r.line_spans.clone(),
                symbol_container: r.symbol_container.clone(),
                symbol_member: r.symbol_member.clone(),
                context: r.context.clone(),
                context_tokens: r.context_tokens.join(" "),
                group_key: g.map(|g| g.key.clone()),
                group_label: g.map(|g| g.label.clone()),
                group_anchor: g.map(|g| g.anchor.clone()),
                declared: r.declared,
            }
        })
        .collect()
}

/// DCB W1.A — record the doc→code references [`crate::coderefs`] extracts.
/// The doc-graph sibling of [`EdgeRecordHook`] (both read `ctx.html`, both
/// record an outbound reference set), which is why it sits next to it; the
/// tables are disjoint, so the order carries no functional coupling — the
/// registry is order-stable on purpose.
///
/// Best-effort like `record_edges`, but deliberately NOT surfaced via
/// `record_failure`: an inert rail section is a lesser failure than a missing
/// edge, and a hashless Errors row per doc on a corpus-wide reindex would
/// drown the Errors tab.
struct CodeRefHook;

impl EnrichmentHook for CodeRefHook {
    fn name(&self) -> &'static str {
        "code-refs"
    }

    /// **ONLY** gate: `memory-session` transcripts are EXCLUDED — the
    /// [`SnapshotCaptureHook`] precedent with the same rationale: a capture's
    /// `<pre>` is escaped JSONL, any session where an agent wrote HTML
    /// contains the literal `<code`, and a full DOM parse of a multi-MB
    /// transcript per capture is exactly the cost `SessionCaptureHook` was
    /// rewritten to avoid. Session→code linkage already has a home
    /// (`kb why` / `session_files`).
    ///
    /// DCB-W1.B.R (operator ruling): a second, cheap substring gate used to
    /// live here too (no `<code`, no GitHub link) — but gating it at
    /// `interested()` meant a prose-only doc got NO `code_refs_docs` row at
    /// all, so its `never_scanned` stayed `true` forever, unfixable by
    /// `kb reindex` (nothing about a reindex changes whether the doc
    /// contains `<code`). That gate has moved INTO [`Self::enrich`]: it
    /// still skips the expensive DOM walk for docs that fail it (the perf
    /// property `interested()` used to provide), but now records an EMPTY
    /// header row instead of no row — so `never_scanned` means exactly
    /// "pre-DCB-index or memory-session", nothing else. See the wire type's
    /// `never_scanned` field doc (`kb_server::routes::coderefs`).
    fn interested(&self, ctx: &EnrichCtx<'_>) -> bool {
        !is_memory_session_category(ctx.kb_category)
    }

    fn enrich<'a>(&'a self, ctx: &'a EnrichCtx<'a>) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            // The substring pre-gate moved from `interested()` (DCB-W1.B.R):
            // `<a href>` issues are admitted too, so a doc that cites only
            // GitHub issues isn't silently skipped. The needle is checked
            // against both the common lowercase spelling and GitHub's own
            // brand capitalization (`GitHub.com`) — `parse_issue_href`
            // (crate::coderefs) matches its host case-insensitively, so a
            // scheme/host-cased href that slips past this check would
            // otherwise parse fine but never get the chance. This is NOT a
            // full case-insensitive scan (an all-caps or otherwise
            // oddly-cased host, e.g. `GITHUB.COM`, still slips through) — a
            // real lowercasing pass would allocate a copy of the whole doc
            // body per enrichment call to close a residual gap neither
            // authoring convention nor a browser's address-bar copy/paste
            // actually produces.
            //
            // A doc that fails this check skips `coderefs::extract`'s DOM
            // walk entirely — the multi-MB-doc perf property is preserved —
            // and gets `Extraction::default()` ("scanned, found nothing")
            // recorded below instead, so it reads "0 refs", never
            // "never scanned".
            let has_code_signal = ctx.html.contains("<code")
                || ctx.html.contains("github.com")
                || ctx.html.contains("GitHub.com");
            // `coderefs::extract` owns its DOM and returns fully-owned data,
            // so the !Send `scraper::Html` is gone before the await below
            // (the `ListAnchorHook` scoped-block precedent, in function form).
            let extraction = if has_code_signal {
                crate::coderefs::extract(ctx.html)
            } else {
                crate::coderefs::Extraction::default()
            };
            let header = CodeRefHeaderRow {
                artifact_id: ctx.artifact_id.as_str().to_string(),
                doc_hash: ctx.content_hash.to_string(),
                // R4 — wall clock at write time, injected once per doc-finish.
                // `record_code_refs` ignores it in the changed/unchanged
                // comparison, so it only ever advances on a real change.
                extracted_at: ctx.now_unix,
                code_rev: extraction
                    .code_rev
                    .as_ref()
                    .map(crate::coderefs::CodeRev::to_wire),
                ref_count: extraction.refs.len() as u32,
                group_count: extraction.groups.len() as u32,
                ungrouped_count: extraction.ungrouped_count,
                truncated: extraction.truncated,
            };
            let rows = to_code_ref_rows(&extraction);
            if let Err(e) = ctx.storage.record_code_refs(header, rows).await {
                tracing::warn!(
                    kb = %ctx.kb_name,
                    path = %ctx.path.display(),
                    error = %e,
                    "record_code_refs failed",
                );
            }
            Ok(())
        })
    }
}

// --- 4. Artifact snapshot capture (Track V) ---------------------------------

/// Default number of content snapshots retained per artifact. Pruned on every
/// capture so the `artifact_snapshots` table stays bounded.
pub const DEFAULT_SNAPSHOT_KEEP: u32 = 25;

/// Track V — capture a content snapshot of the artifact's source each time it
/// re-indexes with changed bytes, so the Versions/Diff timeline has history
/// even when the corpus isn't under git. Insert-if-changed (deduped on the
/// raw-bytes `content_hash`), then prune to [`DEFAULT_SNAPSHOT_KEEP`].
///
/// Gated by `versions_mode.uses_index()` (auto/index/both) so a `git`-only or
/// `off` kb stores nothing, and skips `memory-session` transcripts — storing
/// 25 copies of a multi-MB transcript is the real growth risk. Best-effort:
/// any storage error logs + continues (the lance row already indexed fine).
struct SnapshotCaptureHook;

impl EnrichmentHook for SnapshotCaptureHook {
    fn name(&self) -> &'static str {
        "snapshot-capture"
    }

    fn interested(&self, ctx: &EnrichCtx<'_>) -> bool {
        ctx.versions_mode.uses_index() && !is_memory_session_category(ctx.kb_category)
    }

    fn enrich<'a>(&'a self, ctx: &'a EnrichCtx<'a>) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let aid = ctx.artifact_id.as_str().to_string();
            // Dedup: skip when the newest stored snapshot already has this
            // hash. The indexer's upstream content-hash gate skips identical
            // !force reindexes entirely; this also covers the force=true path
            // and the first index after a daemon restart.
            match ctx.storage.snapshot_latest_hash(aid.clone()).await {
                Ok(Some(h)) if h == ctx.content_hash => return Ok(()),
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(kb = %ctx.kb_name, artifact_id = %aid, error = %e, "snapshot_latest_hash failed");
                    return Ok(());
                }
            }
            if let Err(e) = ctx
                .storage
                .snapshot_insert(
                    aid.clone(),
                    ctx.content_hash.to_string(),
                    ctx.raw_source.to_string(),
                    ctx.mtime_unix,
                )
                .await
            {
                tracing::warn!(kb = %ctx.kb_name, artifact_id = %aid, error = %e, "snapshot_insert failed");
                return Ok(());
            }
            if let Err(e) = ctx
                .storage
                .snapshot_prune(aid.clone(), DEFAULT_SNAPSHOT_KEEP)
                .await
            {
                tracing::warn!(kb = %ctx.kb_name, artifact_id = %aid, error = %e, "snapshot_prune failed");
            }
            Ok(())
        })
    }
}

// --- 5. Reading-list anchor resolution (RL-track, v0.18) --------------------

/// RL3 — re-resolve reading-list entry anchors against the just-committed
/// HTML, persist transitions ON THE ROWS (`anchor_stale` is row state —
/// restart-safe by construction, unlike the comments tracker's in-process
/// map), and refresh the per-section `words` estimate while the parsed
/// HTML is in hand. Emits `list.entry.anchor_stale` on a 0→1 transition
/// and `list.entry.anchor_resolved` on 1→0; steady states are silent.
/// The first await is one indexed sqlite lookup — ~free for artifacts no
/// list references.
struct ListAnchorHook;

impl EnrichmentHook for ListAnchorHook {
    fn name(&self) -> &'static str {
        "list-anchor"
    }

    fn interested(&self, _ctx: &EnrichCtx<'_>) -> bool {
        true
    }

    fn enrich<'a>(&'a self, ctx: &'a EnrichCtx<'a>) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let aid = ctx.artifact_id.as_str().to_string();
            let rows = match ctx.storage.list_entries_for_artifact(aid.clone()).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(kb = %ctx.kb_name, artifact_id = %aid, error = %e,
                        "list_entries_for_artifact failed");
                    return Ok(());
                }
            };
            // (row, now-stale, refreshed words, anchor scope name)
            let mut changed: Vec<(
                crate::storage::sqlite::ListEntryRow,
                bool,
                Option<i64>,
                &'static str,
            )> = Vec::new();
            {
                // ONE parse of ctx.html shared across every entry — the
                // `_with` variants fill this slot on the first DOM-needing
                // anchor (the old per-entry calls parsed the same document
                // up to twice per row). Scoped block: `scraper::Html` is
                // !Send, so the slot must drop before the next `.await`.
                let mut dom: Option<scraper::Html> = None;
                for row in rows {
                    let Some(json) = row.anchor_json.as_deref() else {
                        continue; // whole-artifact entries can't go stale
                    };
                    let (stale_now, words, kind) =
                        match serde_json::from_str::<crate::review::Anchor>(json) {
                            Ok(anchor) => {
                                let stale = matches!(
                                    crate::review::fuzzy_resolve_anchor_with(
                                        &mut dom, ctx.html, &anchor
                                    ),
                                    crate::review::Resolution::Stale
                                );
                                let words = if stale {
                                    None // keep the last-known estimate
                                } else {
                                    crate::lists::section_words_with(&mut dom, ctx.html, &anchor)
                                        .map(i64::from)
                                };
                                (stale, words, anchor.scope_name())
                            }
                            Err(e) => {
                                tracing::warn!(kb = %ctx.kb_name, entry_id = %row.id, error = %e,
                                    "corrupt list-entry anchor JSON; treating as stale");
                                (true, None, "unknown")
                            }
                        };
                    let words_changed = words.is_some() && words != row.words;
                    if stale_now != row.anchor_stale || words_changed {
                        changed.push((row, stale_now, words, kind));
                    }
                }
            }
            if changed.is_empty() {
                return Ok(());
            }
            let updates: Vec<crate::lists::ResolutionUpdate> = changed
                .iter()
                .map(|(row, stale, words, _)| crate::lists::ResolutionUpdate {
                    entry_id: row.id.clone(),
                    anchor_stale: *stale,
                    words: *words,
                })
                .collect();
            if let Err(e) = ctx.storage.list_entries_sync_resolution(updates).await {
                tracing::warn!(kb = %ctx.kb_name, artifact_id = %aid, error = %e,
                    "list_entries_sync_resolution failed");
                return Ok(());
            }
            // Emit transitions only AFTER the rows are persisted, so a
            // subscriber's refetch sees what the event claims.
            for (row, stale_now, _, kind) in &changed {
                if *stale_now && !row.anchor_stale {
                    ctx.bus.emit(
                        "list.entry.anchor_stale",
                        json!({
                            "kb": ctx.kb_name.as_str(),
                            "list_id": row.list_id,
                            "entry_id": row.id,
                            "artifact_id": row.artifact_id,
                            "anchor_kind": kind,
                            "source_relative": ctx.rel_path,
                        }),
                    );
                } else if !*stale_now && row.anchor_stale {
                    ctx.bus.emit(
                        "list.entry.anchor_resolved",
                        json!({
                            "kb": ctx.kb_name.as_str(),
                            "list_id": row.list_id,
                            "entry_id": row.id,
                            "artifact_id": row.artifact_id,
                        }),
                    );
                }
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_hooks_registration_order() {
        // Order IS the run order; pre-X2 inline order was session → memory
        // link → edge. Track V appends snapshot-capture; RL3 appends
        // list-anchor at the end (independent tables — but the registry is
        // order-stable on purpose). MI-W1.1 inserts memory-recall-ledger
        // right after session-capture — same `interested` gate, sibling
        // parse over the same just-captured transcript. DCB W1.A inserts
        // code-refs right after edge-record — the other doc-graph writer over
        // `ctx.html`; disjoint tables, so the position is grouping, not
        // coupling.
        let names: Vec<&str> = default_hooks().iter().map(|h| h.name()).collect();
        assert_eq!(
            names,
            [
                "session-capture",
                "memory-recall-ledger",
                // CT-F1 appends the memory<->commit ledger beside its recall
                // sibling: same `interested` gate, its own derivation (the
                // envelope's commits block) and its own table.
                "memory-commit-ledger",
                "memory-link-seed",
                "edge-record",
                "code-refs",
                "snapshot-capture",
                "list-anchor",
            ]
        );
    }

    #[test]
    fn session_category_predicate() {
        assert!(is_memory_session_category(Some("memory-session")));
        assert!(!is_memory_session_category(Some("memory-fact")));
        assert!(!is_memory_session_category(Some("notes")));
        assert!(!is_memory_session_category(None));
    }

    #[test]
    fn memory_artifact_predicate_excludes_sessions() {
        assert!(is_memory_artifact_category(Some("memory-fact")));
        assert!(is_memory_artifact_category(Some("memory-decision")));
        // Transcripts are deliberately NOT seeded.
        assert!(!is_memory_artifact_category(Some("memory-session")));
        assert!(!is_memory_artifact_category(Some("notes")));
        assert!(!is_memory_artifact_category(None));
    }

    /// W0.2 — the SessionCaptureHook write-through: a transcript with one
    /// completed subagent + one async-launched stub must land both the
    /// stat totals AND the separate unstatted counter in the persisted
    /// `sessions` row (V0024).
    #[tokio::test]
    async fn session_capture_hook_persists_subagent_stats() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = crate::storage::StorageActor::spawn(
            tmp.path().join("lance"),
            tmp.path().join("index.db"),
            None,
        )
        .await
        .unwrap();
        let bus = EventBus::default();
        let kb_name = KbName::new("smoke").unwrap();
        let source_slug = SourceSlug::from_path(tmp.path());
        let path = tmp.path().join("sess.html");
        let artifact_id = ArtifactId::from_path("sess.html");

        // One completed (sync) subagent (500 tokens / 12 tool calls / 2
        // edited) + one async-launched stub with no stats. sessionId is the
        // ground-truth session id (invariant #11) — carried explicitly so
        // the lookup below doesn't depend on filename-stem fallback.
        let jsonl = concat!(
            r#"{"sessionId":"sess-sub-1","type":"user","toolUseResult":{"status":"completed","agentId":"a1","totalTokens":500,"totalToolUseCount":12,"toolStats":{"editFileCount":2}},"message":{"role":"user","content":[{"type":"tool_result","content":"done"}]}}"#,
            "\n",
            r#"{"sessionId":"sess-sub-1","type":"user","toolUseResult":{"status":"async_launched","agentId":"a2"},"message":{"role":"user","content":[{"type":"tool_result","content":"launched"}]}}"#,
            "\n",
        );
        let html = format!(
            "<html><head><meta name=\"kb-category\" content=\"memory-session\"></head>\
             <body><pre>{jsonl}</pre></body></html>"
        );

        let ctx = EnrichCtx {
            kb_name: &kb_name,
            source_slug: &source_slug,
            storage: &storage,
            bus: &bus,
            quarantine_dir: tmp.path(),
            source_root: tmp.path(),
            path: &path,
            artifact_id: &artifact_id,
            rel_path: "sess.html",
            html: &html,
            artifact_host_suffix: crate::iframe::DEFAULT_HOST_SUFFIX,
            kb_category: Some("memory-session"),
            mtime_unix: 1_700_000_000,
            now_unix: 1_700_000_500,
            seed_global: false,
            seed_linked_kbs: &[],
            content_hash: "deadbeef0001",
            raw_source: &html,
            versions_mode: crate::vcs::VersionsMode::Off,
            session_parse: None,
        };

        SessionCaptureHook.enrich(&ctx).await.unwrap();

        let row = storage
            .sessions_get("sess-sub-1".to_string())
            .await
            .unwrap()
            .expect("session row persisted");
        assert_eq!(row.subagent_count, 1);
        assert_eq!(row.subagent_tokens, 500);
        assert_eq!(row.subagent_tool_calls, 12);
        assert_eq!(row.subagent_files_edited, 2);
        assert_eq!(row.subagent_launched_unstatted, 1);
    }

    /// W0.2 — a main-only session (no Agent/Task delegations at all) must
    /// persist plain zeros across every subagent column, distinguishable
    /// from "launched but unstatted" (which would set
    /// `subagent_launched_unstatted` instead).
    #[tokio::test]
    async fn session_capture_hook_zeros_subagent_stats_when_none_ran() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = crate::storage::StorageActor::spawn(
            tmp.path().join("lance"),
            tmp.path().join("index.db"),
            None,
        )
        .await
        .unwrap();
        let bus = EventBus::default();
        let kb_name = KbName::new("smoke").unwrap();
        let source_slug = SourceSlug::from_path(tmp.path());
        let path = tmp.path().join("sess.html");
        let artifact_id = ArtifactId::from_path("sess.html");

        let jsonl = concat!(
            r#"{"sessionId":"sess-main-1","type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}"#,
            "\n",
        );
        let html = format!(
            "<html><head><meta name=\"kb-category\" content=\"memory-session\"></head>\
             <body><pre>{jsonl}</pre></body></html>"
        );

        let ctx = EnrichCtx {
            kb_name: &kb_name,
            source_slug: &source_slug,
            storage: &storage,
            bus: &bus,
            quarantine_dir: tmp.path(),
            source_root: tmp.path(),
            path: &path,
            artifact_id: &artifact_id,
            rel_path: "sess.html",
            html: &html,
            artifact_host_suffix: crate::iframe::DEFAULT_HOST_SUFFIX,
            kb_category: Some("memory-session"),
            mtime_unix: 1_700_000_000,
            now_unix: 1_700_000_500,
            seed_global: false,
            seed_linked_kbs: &[],
            content_hash: "deadbeef0002",
            raw_source: &html,
            versions_mode: crate::vcs::VersionsMode::Off,
            session_parse: None,
        };

        SessionCaptureHook.enrich(&ctx).await.unwrap();

        let row = storage
            .sessions_get("sess-main-1".to_string())
            .await
            .unwrap()
            .expect("session row persisted");
        assert_eq!(row.subagent_count, 0);
        assert_eq!(row.subagent_tokens, 0);
        assert_eq!(row.subagent_tool_calls, 0);
        assert_eq!(row.subagent_files_edited, 0);
        assert_eq!(row.subagent_launched_unstatted, 0);
    }

    /// W0.4 — a capture artifact carrying the additive commits block must
    /// have its session_commits rows carry the RESOLVED values (sha_full,
    /// resolved=1, author, parents, trailers), not just the transcript-
    /// detected kind/sha/subject.
    #[tokio::test]
    async fn session_capture_hook_prefers_the_commits_block_when_present() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = crate::storage::StorageActor::spawn(
            tmp.path().join("lance"),
            tmp.path().join("index.db"),
            None,
        )
        .await
        .unwrap();
        let bus = EventBus::default();
        let kb_name = KbName::new("smoke").unwrap();
        let source_slug = SourceSlug::from_path(tmp.path());
        let path = tmp.path().join("sess.html");
        let artifact_id = ArtifactId::from_path("sess.html");

        // The transcript detects a commit with a SHORT sha + a generic
        // "commit" subject (no -m message recoverable) — what a live
        // capture WITHOUT resolution would carry.
        let jsonl = concat!(
            r#"{"sessionId":"sess-commits-1","type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"git commit -F msg.txt"}}]}}"#,
            "\n",
            r#"{"sessionId":"sess-commits-1","type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"b1","content":"[main abc1234] real subject"}]}}"#,
            "\n",
        );
        let commits_block =
            crate::sessions::render_commits_block(&[crate::sessions::CapturedCommit {
                kind: "commit".to_string(),
                sha: Some("abc1234".to_string()),
                subject: Some("real subject".to_string()),
                resolved: true,
                sha_full: Some("abc1234def5678".to_string()),
                repo_root: Some("/home/u/proj".to_string()),
                author: Some("kb-test <test@kb>".to_string()),
                parents: Some(1),
                trailers: vec!["Kb-Session: sess-commits-1".to_string()],
            }]);
        assert!(!commits_block.is_empty());
        let html = format!(
            "<html><head><meta name=\"kb-category\" content=\"memory-session\"></head>\
             <body><pre>{jsonl}</pre>\n{commits_block}\n</body></html>"
        );

        let ctx = EnrichCtx {
            kb_name: &kb_name,
            source_slug: &source_slug,
            storage: &storage,
            bus: &bus,
            quarantine_dir: tmp.path(),
            source_root: tmp.path(),
            path: &path,
            artifact_id: &artifact_id,
            rel_path: "sess.html",
            html: &html,
            artifact_host_suffix: crate::iframe::DEFAULT_HOST_SUFFIX,
            kb_category: Some("memory-session"),
            mtime_unix: 1_700_000_000,
            now_unix: 1_700_000_500,
            seed_global: false,
            seed_linked_kbs: &[],
            content_hash: "deadbeef0003",
            raw_source: &html,
            versions_mode: crate::vcs::VersionsMode::Off,
            session_parse: None,
        };

        SessionCaptureHook.enrich(&ctx).await.unwrap();

        let commits = storage
            .session_commits_for_session("sess-commits-1".to_string())
            .await
            .unwrap();
        assert_eq!(commits.len(), 1);
        let c = &commits[0];
        assert_eq!(c.sha.as_deref(), Some("abc1234"));
        assert_eq!(c.subject.as_deref(), Some("real subject"));
        assert!(c.resolved);
        assert_eq!(c.sha_full.as_deref(), Some("abc1234def5678"));
        assert_eq!(c.repo_root.as_deref(), Some("/home/u/proj"));
        assert_eq!(c.author.as_deref(), Some("kb-test <test@kb>"));
        assert_eq!(c.parents, Some(1));
        assert_eq!(c.trailers.as_deref(), Some("Kb-Session: sess-commits-1"));
    }

    /// W0.4 — a capture WITHOUT the commits block (old captures, `kb import
    /// claude-history` backfills, any hookless writer) must fall back to
    /// pure transcript detection: the row still lands, but `resolved` is
    /// false and every resolution column is empty — legacy behaviour
    /// preserved byte-for-byte.
    #[tokio::test]
    async fn session_capture_hook_falls_back_to_transcript_only_without_the_block() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = crate::storage::StorageActor::spawn(
            tmp.path().join("lance"),
            tmp.path().join("index.db"),
            None,
        )
        .await
        .unwrap();
        let bus = EventBus::default();
        let kb_name = KbName::new("smoke").unwrap();
        let source_slug = SourceSlug::from_path(tmp.path());
        let path = tmp.path().join("sess.html");
        let artifact_id = ArtifactId::from_path("sess.html");

        let jsonl = concat!(
            r#"{"sessionId":"sess-commits-2","type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"git commit -m \"feat: ship it\""}}]}}"#,
            "\n",
            r#"{"sessionId":"sess-commits-2","type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"b1","content":"[main abc9999] feat: ship it"}]}}"#,
            "\n",
        );
        // No commits block — the html looks exactly like a pre-W0.4 capture.
        let html = format!(
            "<html><head><meta name=\"kb-category\" content=\"memory-session\"></head>\
             <body><pre>{jsonl}</pre></body></html>"
        );
        assert!(crate::sessions::extract_commits_block(&html).is_none());

        let ctx = EnrichCtx {
            kb_name: &kb_name,
            source_slug: &source_slug,
            storage: &storage,
            bus: &bus,
            quarantine_dir: tmp.path(),
            source_root: tmp.path(),
            path: &path,
            artifact_id: &artifact_id,
            rel_path: "sess.html",
            html: &html,
            artifact_host_suffix: crate::iframe::DEFAULT_HOST_SUFFIX,
            kb_category: Some("memory-session"),
            mtime_unix: 1_700_000_000,
            now_unix: 1_700_000_500,
            seed_global: false,
            seed_linked_kbs: &[],
            content_hash: "deadbeef0004",
            raw_source: &html,
            versions_mode: crate::vcs::VersionsMode::Off,
            session_parse: None,
        };

        SessionCaptureHook.enrich(&ctx).await.unwrap();

        let commits = storage
            .session_commits_for_session("sess-commits-2".to_string())
            .await
            .unwrap();
        assert_eq!(commits.len(), 1);
        let c = &commits[0];
        assert_eq!(c.sha.as_deref(), Some("abc9999"));
        assert_eq!(c.subject.as_deref(), Some("feat: ship it"));
        assert!(!c.resolved, "no block ⇒ legacy resolved=0");
        assert!(c.sha_full.is_none());
        assert!(c.repo_root.is_none());
        assert!(c.author.is_none());
        assert!(c.parents.is_none());
        assert!(c.trailers.is_none());
    }

    /// W0.5 — when the envelope carries a subagents digest block, the
    /// aggregate columns become SIDECAR-PRIMARY: they're computed from the
    /// block, not the parent transcript's own (sync-only) `toolUseResult`.
    /// The parent transcript here detects one completed-with-stats agent
    /// (a1, W0.2 numbers) + one async-launched stub (a2, no stats) — the
    /// sidecar block covers BOTH with real numbers that deliberately differ
    /// from a1's parent-transcript stats, proving the block wins.
    #[tokio::test]
    async fn session_capture_hook_uses_sidecar_digest_as_aggregate_primary() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = crate::storage::StorageActor::spawn(
            tmp.path().join("lance"),
            tmp.path().join("index.db"),
            None,
        )
        .await
        .unwrap();
        let bus = EventBus::default();
        let kb_name = KbName::new("smoke").unwrap();
        let source_slug = SourceSlug::from_path(tmp.path());
        let path = tmp.path().join("sess.html");
        let artifact_id = ArtifactId::from_path("sess.html");

        let jsonl = concat!(
            r#"{"sessionId":"sess-sidecar-1","type":"user","toolUseResult":{"status":"completed","agentId":"a1","totalTokens":500,"totalToolUseCount":12,"toolStats":{"editFileCount":2}},"message":{"role":"user","content":[{"type":"tool_result","content":"done"}]}}"#,
            "\n",
            r#"{"sessionId":"sess-sidecar-1","type":"user","toolUseResult":{"status":"async_launched","agentId":"a2"},"message":{"role":"user","content":[{"type":"tool_result","content":"launched"}]}}"#,
            "\n",
        );
        let agents = vec![
            crate::sessions::SubagentDigest {
                agent_id: "a1".to_string(),
                files: vec![crate::sessions::SubagentFileEntry {
                    path: "/p/a.rs".to_string(),
                    action: "edit".to_string(),
                }],
                tokens: 900, // deliberately != the parent's 500, to prove the block wins
                tool_calls: 20,
                errors: 0,
            },
            crate::sessions::SubagentDigest {
                agent_id: "a2".to_string(),
                files: vec![
                    crate::sessions::SubagentFileEntry {
                        path: "/p/b.rs".to_string(),
                        action: "write".to_string(),
                    },
                    crate::sessions::SubagentFileEntry {
                        path: "/p/readonly.rs".to_string(),
                        action: "read".to_string(),
                    },
                ],
                tokens: 300, // the async stub's real numbers, invisible to the parent
                tool_calls: 8,
                errors: 1,
            },
        ];
        let subagents_block = crate::sessions::render_subagents_block(&agents);
        let html = format!(
            "<html><head><meta name=\"kb-category\" content=\"memory-session\"></head>\
             <body><pre>{jsonl}</pre>\n{subagents_block}\n</body></html>"
        );

        let ctx = EnrichCtx {
            kb_name: &kb_name,
            source_slug: &source_slug,
            storage: &storage,
            bus: &bus,
            quarantine_dir: tmp.path(),
            source_root: tmp.path(),
            path: &path,
            artifact_id: &artifact_id,
            rel_path: "sess.html",
            html: &html,
            artifact_host_suffix: crate::iframe::DEFAULT_HOST_SUFFIX,
            kb_category: Some("memory-session"),
            mtime_unix: 1_700_000_000,
            now_unix: 1_700_000_500,
            seed_global: false,
            seed_linked_kbs: &[],
            content_hash: "deadbeef0005",
            raw_source: &html,
            versions_mode: crate::vcs::VersionsMode::Off,
            session_parse: None,
        };

        SessionCaptureHook.enrich(&ctx).await.unwrap();

        let row = storage
            .sessions_get("sess-sidecar-1".to_string())
            .await
            .unwrap()
            .expect("session row persisted");
        assert_eq!(row.subagent_count, 2, "both agents matched in sidecars");
        assert_eq!(
            row.subagent_tokens,
            900 + 300,
            "sidecar totals, not the parent's 500"
        );
        assert_eq!(row.subagent_tool_calls, 20 + 8);
        assert_eq!(
            row.subagent_files_edited, 2,
            "edit + write; the read touch doesn't count"
        );
        // Parent-detected launches (1 completed + 1 unstatted = 2) minus
        // matched-in-sidecars (2) = 0.
        assert_eq!(row.subagent_launched_unstatted, 0);
    }

    /// W0.5 — a parent transcript that detected MORE launches than the
    /// sidecar walk matched (e.g. a sidecar pruned before capture) keeps a
    /// non-zero `subagent_launched_unstatted`, floored at 0 rather than
    /// going negative.
    #[tokio::test]
    async fn session_capture_hook_sidecar_primary_unstatted_floors_at_zero() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = crate::storage::StorageActor::spawn(
            tmp.path().join("lance"),
            tmp.path().join("index.db"),
            None,
        )
        .await
        .unwrap();
        let bus = EventBus::default();
        let kb_name = KbName::new("smoke").unwrap();
        let source_slug = SourceSlug::from_path(tmp.path());
        let path = tmp.path().join("sess.html");
        let artifact_id = ArtifactId::from_path("sess.html");

        // Parent transcript detects THREE launches (1 completed + 2 stubs);
        // the sidecar block matches only ONE of them.
        let jsonl = concat!(
            r#"{"sessionId":"sess-sidecar-2","type":"user","toolUseResult":{"status":"completed","agentId":"a1","totalTokens":100,"totalToolUseCount":5,"toolStats":{"editFileCount":0}},"message":{"role":"user","content":[{"type":"tool_result","content":"done"}]}}"#,
            "\n",
            r#"{"sessionId":"sess-sidecar-2","type":"user","toolUseResult":{"status":"async_launched","agentId":"a2"},"message":{"role":"user","content":[{"type":"tool_result","content":"launched"}]}}"#,
            "\n",
            r#"{"sessionId":"sess-sidecar-2","type":"user","toolUseResult":{"status":"async_launched","agentId":"a3"},"message":{"role":"user","content":[{"type":"tool_result","content":"launched"}]}}"#,
            "\n",
        );
        let agents = vec![crate::sessions::SubagentDigest {
            agent_id: "a1".to_string(),
            files: vec![],
            tokens: 100,
            tool_calls: 5,
            errors: 0,
        }];
        let subagents_block = crate::sessions::render_subagents_block(&agents);
        let html = format!(
            "<html><head><meta name=\"kb-category\" content=\"memory-session\"></head>\
             <body><pre>{jsonl}</pre>\n{subagents_block}\n</body></html>"
        );

        let ctx = EnrichCtx {
            kb_name: &kb_name,
            source_slug: &source_slug,
            storage: &storage,
            bus: &bus,
            quarantine_dir: tmp.path(),
            source_root: tmp.path(),
            path: &path,
            artifact_id: &artifact_id,
            rel_path: "sess.html",
            html: &html,
            artifact_host_suffix: crate::iframe::DEFAULT_HOST_SUFFIX,
            kb_category: Some("memory-session"),
            mtime_unix: 1_700_000_000,
            now_unix: 1_700_000_500,
            seed_global: false,
            seed_linked_kbs: &[],
            content_hash: "deadbeef0006",
            raw_source: &html,
            versions_mode: crate::vcs::VersionsMode::Off,
            session_parse: None,
        };

        SessionCaptureHook.enrich(&ctx).await.unwrap();

        let row = storage
            .sessions_get("sess-sidecar-2".to_string())
            .await
            .unwrap()
            .expect("session row persisted");
        assert_eq!(row.subagent_count, 1, "only a1 has a sidecar");
        // Launched-per-parent (1 completed + 2 unstatted = 3) minus
        // matched-in-sidecars (1) = 2.
        assert_eq!(row.subagent_launched_unstatted, 2);
    }

    /// W0.5 — sidecar file touches merge into `session_files` alongside the
    /// main thread's own, with the MAIN-THREAD row winning a (path, action)
    /// collision (its copy of `/p/shared.rs` read survives; the subagent's
    /// duplicate copy of the same touch is dropped, not overwritten) and
    /// every subagent-only touch persisting with `via_subagent = true`.
    #[tokio::test]
    async fn session_capture_hook_merges_subagent_files_main_thread_wins_collision() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = crate::storage::StorageActor::spawn(
            tmp.path().join("lance"),
            tmp.path().join("index.db"),
            None,
        )
        .await
        .unwrap();
        let bus = EventBus::default();
        let kb_name = KbName::new("smoke").unwrap();
        let source_slug = SourceSlug::from_path(tmp.path());
        let path = tmp.path().join("sess.html");
        let artifact_id = ArtifactId::from_path("sess.html");

        // Main thread reads /p/shared.rs and edits /p/main-only.rs.
        let jsonl = concat!(
            r#"{"sessionId":"sess-sidecar-3","type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Read","input":{"file_path":"/p/shared.rs"}},{"type":"tool_use","name":"Edit","input":{"file_path":"/p/main-only.rs"}}]}}"#,
            "\n",
        );
        // The subagent ALSO reads /p/shared.rs (collision — must be dropped)
        // and additionally edits /p/agent-only.rs (must survive, via_subagent=true).
        let agents = vec![crate::sessions::SubagentDigest {
            agent_id: "a1".to_string(),
            files: vec![
                crate::sessions::SubagentFileEntry {
                    path: "/p/shared.rs".to_string(),
                    action: "read".to_string(),
                },
                crate::sessions::SubagentFileEntry {
                    path: "/p/agent-only.rs".to_string(),
                    action: "edit".to_string(),
                },
            ],
            tokens: 10,
            tool_calls: 2,
            errors: 0,
        }];
        let subagents_block = crate::sessions::render_subagents_block(&agents);
        let html = format!(
            "<html><head><meta name=\"kb-category\" content=\"memory-session\"></head>\
             <body><pre>{jsonl}</pre>\n{subagents_block}\n</body></html>"
        );

        let ctx = EnrichCtx {
            kb_name: &kb_name,
            source_slug: &source_slug,
            storage: &storage,
            bus: &bus,
            quarantine_dir: tmp.path(),
            source_root: tmp.path(),
            path: &path,
            artifact_id: &artifact_id,
            rel_path: "sess.html",
            html: &html,
            artifact_host_suffix: crate::iframe::DEFAULT_HOST_SUFFIX,
            kb_category: Some("memory-session"),
            mtime_unix: 1_700_000_000,
            now_unix: 1_700_000_500,
            seed_global: false,
            seed_linked_kbs: &[],
            content_hash: "deadbeef0007",
            raw_source: &html,
            versions_mode: crate::vcs::VersionsMode::Off,
            session_parse: None,
        };

        SessionCaptureHook.enrich(&ctx).await.unwrap();

        let files = storage
            .session_files_for_session("sess-sidecar-3".to_string())
            .await
            .unwrap();
        assert_eq!(files.len(), 3, "shared.rs collision dedupes to ONE row");

        let shared = files.iter().find(|f| f.path == "/p/shared.rs").unwrap();
        assert!(
            !shared.via_subagent,
            "main-thread row wins the collision, not the subagent's duplicate"
        );

        let main_only = files.iter().find(|f| f.path == "/p/main-only.rs").unwrap();
        assert!(!main_only.via_subagent);

        let agent_only = files.iter().find(|f| f.path == "/p/agent-only.rs").unwrap();
        assert!(
            agent_only.via_subagent,
            "the subagent-only touch persists, flagged"
        );
    }

    /// W0.5 — a session with NO subagents block behaves EXACTLY as before
    /// (W0.2/pre-W0.5): every `session_files` row is main-thread
    /// (`via_subagent = false`), and the aggregate columns come from the
    /// parent-transcript-only fallback. Pins zero behavior change on the
    /// no-sidecar path.
    #[tokio::test]
    async fn session_capture_hook_without_subagents_block_is_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = crate::storage::StorageActor::spawn(
            tmp.path().join("lance"),
            tmp.path().join("index.db"),
            None,
        )
        .await
        .unwrap();
        let bus = EventBus::default();
        let kb_name = KbName::new("smoke").unwrap();
        let source_slug = SourceSlug::from_path(tmp.path());
        let path = tmp.path().join("sess.html");
        let artifact_id = ArtifactId::from_path("sess.html");

        let jsonl = concat!(
            r#"{"sessionId":"sess-nosidecar-1","type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Read","input":{"file_path":"/p/only.rs"}}]}}"#,
            "\n",
            r#"{"sessionId":"sess-nosidecar-1","type":"user","toolUseResult":{"status":"completed","agentId":"a1","totalTokens":42,"totalToolUseCount":3,"toolStats":{"editFileCount":0}},"message":{"role":"user","content":[{"type":"tool_result","content":"done"}]}}"#,
            "\n",
        );
        // No subagents block appended — looks like every pre-W0.5 capture.
        let html = format!(
            "<html><head><meta name=\"kb-category\" content=\"memory-session\"></head>\
             <body><pre>{jsonl}</pre></body></html>"
        );
        assert!(crate::sessions::extract_subagents_block(&html).is_none());

        let ctx = EnrichCtx {
            kb_name: &kb_name,
            source_slug: &source_slug,
            storage: &storage,
            bus: &bus,
            quarantine_dir: tmp.path(),
            source_root: tmp.path(),
            path: &path,
            artifact_id: &artifact_id,
            rel_path: "sess.html",
            html: &html,
            artifact_host_suffix: crate::iframe::DEFAULT_HOST_SUFFIX,
            kb_category: Some("memory-session"),
            mtime_unix: 1_700_000_000,
            now_unix: 1_700_000_500,
            seed_global: false,
            seed_linked_kbs: &[],
            content_hash: "deadbeef0008",
            raw_source: &html,
            versions_mode: crate::vcs::VersionsMode::Off,
            session_parse: None,
        };

        SessionCaptureHook.enrich(&ctx).await.unwrap();

        let row = storage
            .sessions_get("sess-nosidecar-1".to_string())
            .await
            .unwrap()
            .expect("session row persisted");
        assert_eq!(row.subagent_count, 1, "parent-only fallback, unchanged");
        assert_eq!(row.subagent_tokens, 42);
        assert_eq!(row.subagent_launched_unstatted, 0);

        let files = storage
            .session_files_for_session("sess-nosidecar-1".to_string())
            .await
            .unwrap();
        assert_eq!(files.len(), 1);
        assert!(
            !files[0].via_subagent,
            "no block ⇒ every row is main-thread"
        );
    }

    // --- MemoryRecallLedgerHook (MI-W1.1) ----------------------------------

    /// A transcript whose captured JSONL carries a recall-hook injection
    /// (with a `↳` summary continuation) must land exactly one
    /// `memory_recalls` row per hit, ids/kb parsed from the injected text.
    #[tokio::test]
    async fn memory_recall_ledger_hook_persists_derived_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = crate::storage::StorageActor::spawn(
            tmp.path().join("lance"),
            tmp.path().join("index.db"),
            None,
        )
        .await
        .unwrap();
        let bus = EventBus::default();
        let kb_name = KbName::new("smoke").unwrap();
        let source_slug = SourceSlug::from_path(tmp.path());
        let path = tmp.path().join("sess.html");
        let artifact_id = ArtifactId::from_path("sess.html");

        let jsonl = concat!(
            r#"{"sessionId":"sess-recall-1","parentUuid":null,"isSidechain":false,"attachment":{"type":"hook_additional_context","content":["Relevant memories from kb (recall - these persist across sessions):\n- ingest retry cap (2026-05-02)  [notes]  (id 4c1d9a77bb21)\n    ↳ Three attempts then park the batch."],"hookName":"UserPromptSubmit","toolUseID":"UserPromptSubmit","hookEvent":"UserPromptSubmit"},"type":"attachment","uuid":"30000001-0000-4000-8000-000000000001","timestamp":"2026-06-04T09:00:05.000Z"}"#,
            "\n",
            r#"{"sessionId":"sess-recall-1","isSidechain":false,"type":"user","message":{"role":"user","content":"go"},"uuid":"30000002-0000-4000-8000-000000000002","parentUuid":"30000001-0000-4000-8000-000000000001","timestamp":"2026-06-04T09:00:25.000Z"}"#,
            "\n",
        );
        let html = format!(
            "<html><head><meta name=\"kb-category\" content=\"memory-session\"></head>\
             <body><pre>{jsonl}</pre></body></html>"
        );

        let ctx = EnrichCtx {
            kb_name: &kb_name,
            source_slug: &source_slug,
            storage: &storage,
            bus: &bus,
            quarantine_dir: tmp.path(),
            source_root: tmp.path(),
            path: &path,
            artifact_id: &artifact_id,
            rel_path: "sess.html",
            html: &html,
            artifact_host_suffix: crate::iframe::DEFAULT_HOST_SUFFIX,
            kb_category: Some("memory-session"),
            mtime_unix: 1_700_000_000,
            now_unix: 1_700_000_500,
            seed_global: false,
            seed_linked_kbs: &[],
            content_hash: "deadbeef0009",
            raw_source: &html,
            versions_mode: crate::vcs::VersionsMode::Off,
            session_parse: None,
        };

        // `memory_recalls_for_session` now resolves the newest capture via
        // a join back to `sessions` (review-fix, capture-scoped ledger) —
        // run `SessionCaptureHook` first, exactly as `default_hooks()`
        // orders them in production, so that row exists.
        SessionCaptureHook.enrich(&ctx).await.unwrap();
        MemoryRecallLedgerHook.enrich(&ctx).await.unwrap();

        let rows = storage
            .memory_recalls_for_session("sess-recall-1".to_string())
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "one row for the one recalled hit");
        assert_eq!(rows[0].memory_kb, "notes");
        assert_eq!(rows[0].memory_id, "4c1d9a77bb21");
        assert_eq!(rows[0].session_id, "sess-recall-1");
        assert_eq!(rows[0].artifact_id, artifact_id.as_str());
        assert!(rows[0].turn_id.is_some());
        assert!(rows[0].recalled_at.is_some());

        // CT-F5 — the SAME pass persists the CT-A3 parse census (V0039).
        // Before this, the counters were ndjson-logged only, which made the
        // ledger parse-failure SLO unmeasurable. This hit has no
        // `kb-recall/1` marker, so it lands in `fallback_parsed`.
        assert_eq!(
            storage.sessions_recall_census_totals().await.unwrap(),
            (0, 1, 0, 1),
            "one censused capture: 0 marker-parsed, 1 fallback-parsed, 0 failed"
        );
    }

    /// CT-F5 — an injected hit that parses via NEITHER grammar must be
    /// counted (`failed`) rather than vanishing. That silent drop is the
    /// exact failure the SLO exists to make visible: memories were shown to
    /// an agent and no ledger row says so.
    #[tokio::test]
    async fn memory_recall_ledger_hook_censuses_a_hit_that_parses_via_neither_grammar() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = crate::storage::StorageActor::spawn(
            tmp.path().join("lance"),
            tmp.path().join("index.db"),
            None,
        )
        .await
        .unwrap();
        let bus = EventBus::default();
        let kb_name = KbName::new("smoke").unwrap();
        let source_slug = SourceSlug::from_path(tmp.path());
        let path = tmp.path().join("sess.html");
        let artifact_id = ArtifactId::from_path("sess.html");

        // An injection whose item matches neither the marker nor the
        // free-text `(id <hex12>)` grammar.
        let jsonl = concat!(
            r#"{"sessionId":"sess-census-1","parentUuid":null,"isSidechain":false,"attachment":{"type":"hook_additional_context","content":["Relevant memories from kb (recall - these persist across sessions):\n- something the grammar cannot parse at all"],"hookName":"UserPromptSubmit","toolUseID":"UserPromptSubmit","hookEvent":"UserPromptSubmit"},"type":"attachment","uuid":"30000001-0000-4000-8000-000000000001","timestamp":"2026-06-04T09:00:05.000Z"}"#,
            "\n",
            r#"{"sessionId":"sess-census-1","isSidechain":false,"type":"user","message":{"role":"user","content":"go"},"uuid":"30000002-0000-4000-8000-000000000002","parentUuid":"30000001-0000-4000-8000-000000000001","timestamp":"2026-06-04T09:00:25.000Z"}"#,
            "\n",
        );
        let html = format!(
            "<html><head><meta name=\"kb-category\" content=\"memory-session\"></head>\
             <body><pre>{jsonl}</pre></body></html>"
        );
        let ctx = EnrichCtx {
            kb_name: &kb_name,
            source_slug: &source_slug,
            storage: &storage,
            bus: &bus,
            quarantine_dir: tmp.path(),
            source_root: tmp.path(),
            path: &path,
            artifact_id: &artifact_id,
            rel_path: "sess.html",
            html: &html,
            artifact_host_suffix: crate::iframe::DEFAULT_HOST_SUFFIX,
            kb_category: Some("memory-session"),
            mtime_unix: 1_700_000_000,
            now_unix: 1_700_000_500,
            seed_global: false,
            seed_linked_kbs: &[],
            content_hash: "deadbeef000a",
            raw_source: &html,
            versions_mode: crate::vcs::VersionsMode::Off,
            session_parse: None,
        };
        SessionCaptureHook.enrich(&ctx).await.unwrap();
        MemoryRecallLedgerHook.enrich(&ctx).await.unwrap();

        assert!(
            storage
                .memory_recalls_for_session("sess-census-1".to_string())
                .await
                .unwrap()
                .is_empty(),
            "the unparseable hit yields no ledger row — that IS the gap"
        );
        assert_eq!(
            storage.sessions_recall_census_totals().await.unwrap(),
            (0, 0, 1, 1),
            "…and the gap is now COUNTED, not merely logged"
        );

        // End-to-end through the pure layer: a 100% failure rate, surfaced.
        let (m, f, x, n) = storage.sessions_recall_census_totals().await.unwrap();
        let report = crate::slo::build(
            "smoke",
            &crate::slo::SloInputs {
                recall_marker_parsed: m,
                recall_fallback_parsed: f,
                recall_failed: x,
                recall_censused_captures: n,
                ..Default::default()
            },
            &crate::slo::SloTargets {
                ledger_parse_failure_pct: Some(1.0),
                ..Default::default()
            },
            1_700_000_500,
        );
        let ind = report
            .indicators
            .iter()
            .find(|i| i.key == "ledger_parse_failure_pct")
            .unwrap();
        assert_eq!(ind.value, Some(100.0));
        assert_eq!(ind.status, crate::slo::SloStatus::Warn);
    }

    /// MI-W1.1 — re-running the hook on the SAME (re-captured) transcript
    /// must REPLACE, not accumulate: the row count for the session stays
    /// exactly what the latest derivation produces, never doubling.
    #[tokio::test]
    async fn memory_recall_ledger_hook_is_idempotent_across_recapture() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = crate::storage::StorageActor::spawn(
            tmp.path().join("lance"),
            tmp.path().join("index.db"),
            None,
        )
        .await
        .unwrap();
        let bus = EventBus::default();
        let kb_name = KbName::new("smoke").unwrap();
        let source_slug = SourceSlug::from_path(tmp.path());
        let path = tmp.path().join("sess.html");
        let artifact_id = ArtifactId::from_path("sess.html");

        let jsonl = concat!(
            r#"{"sessionId":"sess-recall-2","parentUuid":null,"isSidechain":false,"attachment":{"type":"hook_additional_context","content":["Relevant memories from kb (recall - these persist across sessions):\n- alpha  [notes]  (id aaaaaaaaaaaa)\n- beta  [notes]  (id bbbbbbbbbbbb)"],"hookName":"UserPromptSubmit","toolUseID":"UserPromptSubmit","hookEvent":"UserPromptSubmit"},"type":"attachment","uuid":"31000001-0000-4000-8000-000000000001","timestamp":"2026-06-04T09:00:05.000Z"}"#,
            "\n",
            r#"{"sessionId":"sess-recall-2","isSidechain":false,"type":"user","message":{"role":"user","content":"go"},"uuid":"31000002-0000-4000-8000-000000000002","parentUuid":"31000001-0000-4000-8000-000000000001","timestamp":"2026-06-04T09:00:25.000Z"}"#,
            "\n",
        );
        let html = format!(
            "<html><head><meta name=\"kb-category\" content=\"memory-session\"></head>\
             <body><pre>{jsonl}</pre></body></html>"
        );

        let ctx = EnrichCtx {
            kb_name: &kb_name,
            source_slug: &source_slug,
            storage: &storage,
            bus: &bus,
            quarantine_dir: tmp.path(),
            source_root: tmp.path(),
            path: &path,
            artifact_id: &artifact_id,
            rel_path: "sess.html",
            html: &html,
            artifact_host_suffix: crate::iframe::DEFAULT_HOST_SUFFIX,
            kb_category: Some("memory-session"),
            mtime_unix: 1_700_000_000,
            now_unix: 1_700_000_500,
            seed_global: false,
            seed_linked_kbs: &[],
            content_hash: "deadbeef000a",
            raw_source: &html,
            versions_mode: crate::vcs::VersionsMode::Off,
            session_parse: None,
        };

        // `memory_recalls_for_session` now resolves the newest capture via
        // a join back to `sessions` (review-fix, capture-scoped ledger) —
        // run `SessionCaptureHook` first, exactly as `default_hooks()`
        // orders them in production, so that row exists.
        SessionCaptureHook.enrich(&ctx).await.unwrap();
        // Run the SAME derivation twice — as a re-index of an unchanged
        // file, or a second Stop-hook capture with identical content, would.
        MemoryRecallLedgerHook.enrich(&ctx).await.unwrap();
        MemoryRecallLedgerHook.enrich(&ctx).await.unwrap();

        let rows = storage
            .memory_recalls_for_session("sess-recall-2".to_string())
            .await
            .unwrap();
        assert_eq!(
            rows.len(),
            2,
            "delete-then-insert replace: still exactly 2 rows, never 4"
        );
    }

    /// A `Write` sink over a shared buffer, so a test can install a scoped
    /// `tracing` subscriber (`tracing::subscriber::set_default`, thread-local
    /// — safe across `.await` on `#[tokio::test]`'s single-threaded runtime)
    /// and assert on the rendered log line, without touching the process's
    /// global dispatcher.
    #[derive(Clone)]
    struct CaptureWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// CT-A3 — a mangled injection (no `kb-recall/1` marker AND no
    /// parseable `(id …)` free-text line either) must count as `failed`,
    /// persist NO row, and emit the parse-gap `tracing::warn!` naming the
    /// session id and counts — the greppable signal that a reformatted (or
    /// hand-edited) `kb-recall.sh` block is never a silent zero in the
    /// `memory_recalls` ledger.
    #[tokio::test]
    async fn memory_recall_ledger_hook_warns_on_a_mangled_injection() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = crate::storage::StorageActor::spawn(
            tmp.path().join("lance"),
            tmp.path().join("index.db"),
            None,
        )
        .await
        .unwrap();
        let bus = EventBus::default();
        let kb_name = KbName::new("smoke").unwrap();
        let source_slug = SourceSlug::from_path(tmp.path());
        let path = tmp.path().join("sess.html");
        let artifact_id = ArtifactId::from_path("sess.html");

        let jsonl = concat!(
            r#"{"sessionId":"sess-recall-mangled","parentUuid":null,"isSidechain":false,"attachment":{"type":"hook_additional_context","content":["Relevant memories from kb (recall - these persist across sessions):\n- a hand-edited line with no brackets at all"],"hookName":"UserPromptSubmit","toolUseID":"UserPromptSubmit","hookEvent":"UserPromptSubmit"},"type":"attachment","uuid":"32000001-0000-4000-8000-000000000001","timestamp":"2026-06-04T09:00:05.000Z"}"#,
            "\n",
            r#"{"sessionId":"sess-recall-mangled","isSidechain":false,"type":"user","message":{"role":"user","content":"go"},"uuid":"32000002-0000-4000-8000-000000000002","parentUuid":"32000001-0000-4000-8000-000000000001","timestamp":"2026-06-04T09:00:25.000Z"}"#,
            "\n",
        );
        let html = format!(
            "<html><head><meta name=\"kb-category\" content=\"memory-session\"></head>\
             <body><pre>{jsonl}</pre></body></html>"
        );

        let ctx = EnrichCtx {
            kb_name: &kb_name,
            source_slug: &source_slug,
            storage: &storage,
            bus: &bus,
            quarantine_dir: tmp.path(),
            source_root: tmp.path(),
            path: &path,
            artifact_id: &artifact_id,
            rel_path: "sess.html",
            html: &html,
            artifact_host_suffix: crate::iframe::DEFAULT_HOST_SUFFIX,
            kb_category: Some("memory-session"),
            mtime_unix: 1_700_000_000,
            now_unix: 1_700_000_500,
            seed_global: false,
            seed_linked_kbs: &[],
            content_hash: "deadbeef000b",
            raw_source: &html,
            versions_mode: crate::vcs::VersionsMode::Off,
            session_parse: None,
        };

        SessionCaptureHook.enrich(&ctx).await.unwrap();

        let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
        let buf_writer = buf.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || CaptureWriter(buf_writer.clone()))
            .with_ansi(false)
            .finish();
        let guard = tracing::subscriber::set_default(subscriber);
        MemoryRecallLedgerHook.enrich(&ctx).await.unwrap();
        drop(guard);

        let rows = storage
            .memory_recalls_for_session("sess-recall-mangled".to_string())
            .await
            .unwrap();
        assert!(rows.is_empty(), "the one mangled hit produced no row");

        let logged = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(
            logged.contains("memory recall injection parse gap"),
            "expected the parse-gap warn, got: {logged}"
        );
        assert!(
            logged.contains("sess-recall-mangled"),
            "warn must name the session id: {logged}"
        );
        assert!(
            logged.contains("failed=1"),
            "warn must carry the failed count: {logged}"
        );
    }

    // --- MemoryCommitLedgerHook (CT-F1) ------------------------------------

    /// One `memory-session` capture whose envelope tail carries a commits
    /// block. `commits` is rendered by the SAME `render_commits_block` the
    /// capture pipeline uses, so the fixture can't drift from the wire.
    fn commit_capture_html(
        session_id: &str,
        commits: &[crate::sessions::CapturedCommit],
    ) -> String {
        let jsonl = format!(
            r#"{{"sessionId":"{session_id}","isSidechain":false,"type":"user","message":{{"role":"user","content":"go"}},"uuid":"40000001-0000-4000-8000-000000000001","parentUuid":null,"timestamp":"2026-06-04T09:00:25.000Z"}}"#
        );
        format!(
            "<html><head><meta name=\"kb-category\" content=\"memory-session\"></head>\
             <body><pre>{jsonl}\n</pre>{}</body></html>",
            crate::sessions::render_commits_block(commits)
        )
    }

    fn resolved_commit(sha_full: &str, trailers: &[&str]) -> crate::sessions::CapturedCommit {
        crate::sessions::CapturedCommit {
            kind: "commit".to_string(),
            sha: Some(sha_full[..8].to_string()),
            subject: Some("fix the thing".to_string()),
            resolved: true,
            sha_full: Some(sha_full.to_string()),
            repo_root: Some("/home/user/project/kb".to_string()),
            author: Some("Carol <carol@example.com>".to_string()),
            parents: Some(1),
            trailers: trailers.iter().map(|t| t.to_string()).collect(),
        }
    }

    /// Run `MemoryCommitLedgerHook` (after `SessionCaptureHook`, exactly as
    /// `default_hooks()` orders them) over one capture's html.
    async fn run_commit_ledger(
        storage: &crate::storage::StorageHandle,
        tmp: &std::path::Path,
        rel: &str,
        html: &str,
    ) {
        let bus = EventBus::default();
        let kb_name = KbName::new("smoke").unwrap();
        let source_slug = SourceSlug::from_path(tmp);
        let path = tmp.join(rel);
        let artifact_id = ArtifactId::from_path(rel);
        let ctx = EnrichCtx {
            kb_name: &kb_name,
            source_slug: &source_slug,
            storage,
            bus: &bus,
            quarantine_dir: tmp,
            source_root: tmp,
            path: &path,
            artifact_id: &artifact_id,
            rel_path: rel,
            html,
            artifact_host_suffix: crate::iframe::DEFAULT_HOST_SUFFIX,
            kb_category: Some("memory-session"),
            mtime_unix: 1_700_000_000,
            now_unix: 1_700_000_500,
            seed_global: false,
            seed_linked_kbs: &[],
            content_hash: "deadbeef0009",
            raw_source: html,
            versions_mode: crate::vcs::VersionsMode::Off,
            session_parse: None,
        };
        SessionCaptureHook.enrich(&ctx).await.unwrap();
        MemoryCommitLedgerHook.enrich(&ctx).await.unwrap();
    }

    async fn commit_ledger_storage(tmp: &std::path::Path) -> crate::storage::StorageHandle {
        crate::storage::StorageActor::spawn(tmp.join("lance"), tmp.join("index.db"), None)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn memory_commit_ledger_hook_persists_one_row_per_trailer() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = commit_ledger_storage(tmp.path()).await;
        let html = commit_capture_html(
            "sess-commit-1",
            &[resolved_commit(
                "deadbeef00112233445566778899aabbccddeeff",
                &[
                    "Kb-Session: sess-commit-1",
                    "Kb-Memory: abc123def456",
                    "Kb-Memory: 0123456789ab",
                ],
            )],
        );
        run_commit_ledger(&storage, tmp.path(), "sess.html", &html).await;

        let rows = storage
            .memory_commits_for_memory("abc123def456".to_string(), 10)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].sha_full, "deadbeef00112233445566778899aabbccddeeff");
        assert_eq!(rows[0].sha.as_deref(), Some("deadbeef"));
        assert_eq!(rows[0].subject.as_deref(), Some("fix the thing"));
        assert_eq!(rows[0].repo_root.as_deref(), Some("/home/user/project/kb"));
        assert_eq!(rows[0].session_id, "sess-commit-1");
        assert_eq!(
            rows[0].artifact_id,
            ArtifactId::from_path("sess.html").as_str()
        );
        // `recorded_at` is the INJECTED indexer clock, never a git timestamp.
        assert_eq!(rows[0].recorded_at, 1_700_000_500);
        // The second trailer got its own row against the same commit.
        assert_eq!(
            storage
                .memory_commits_for_memory("0123456789ab".to_string(), 10)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    /// The default-OFF case (the repo never opted into `kb.memoryTrailers`):
    /// a perfectly ordinary capture, `Kb-Session:` trailer and all, writes
    /// nothing — and must not fail the index.
    #[tokio::test]
    async fn memory_commit_ledger_hook_writes_nothing_without_kb_memory_trailers() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = commit_ledger_storage(tmp.path()).await;
        let html = commit_capture_html(
            "sess-commit-2",
            &[resolved_commit(
                "deadbeef00112233445566778899aabbccddeeff",
                &["Kb-Session: sess-commit-2", "Signed-off-by: N <n@e.com>"],
            )],
        );
        run_commit_ledger(&storage, tmp.path(), "sess.html", &html).await;
        assert!(storage
            .memory_commits_for_memory("abc123def456".to_string(), 10)
            .await
            .unwrap()
            .is_empty());
    }

    /// No commits block at all (an old capture / an import backfill) is a
    /// no-op, not a panic and not a wipe.
    #[tokio::test]
    async fn memory_commit_ledger_hook_tolerates_a_capture_with_no_commits_block() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = commit_ledger_storage(tmp.path()).await;
        let html = commit_capture_html("sess-commit-3", &[]);
        assert!(
            !html.contains("kb-session-commits"),
            "render_commits_block emits nothing for an empty slice"
        );
        run_commit_ledger(&storage, tmp.path(), "sess.html", &html).await;
        assert!(storage
            .memory_commits_for_memory("abc123def456".to_string(), 10)
            .await
            .unwrap()
            .is_empty());
    }

    /// An exact join needs an exact sha: a push/tag row and an UNRESOLVED
    /// commit are both dropped even when they carry a `Kb-Memory:` trailer.
    #[tokio::test]
    async fn memory_commit_ledger_hook_skips_unresolved_and_non_commit_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = commit_ledger_storage(tmp.path()).await;
        let mut push = resolved_commit(
            "aa00112233445566778899aabbccddeeff001122",
            &["Kb-Memory: abc123def456"],
        );
        push.kind = "push".to_string();
        let mut unresolved = resolved_commit(
            "bb00112233445566778899aabbccddeeff001122",
            &["Kb-Memory: abc123def456"],
        );
        unresolved.resolved = false;
        unresolved.sha_full = None;
        let html = commit_capture_html("sess-commit-4", &[push, unresolved]);
        run_commit_ledger(&storage, tmp.path(), "sess.html", &html).await;
        assert!(storage
            .memory_commits_for_memory("abc123def456".to_string(), 10)
            .await
            .unwrap()
            .is_empty());
    }

    /// Invariant #11's multi-capture fan-out, end-to-end: the same session
    /// re-captured under a NEW artifact id must still leave exactly one row
    /// per (memory, commit) fact.
    #[tokio::test]
    async fn memory_commit_ledger_hook_is_idempotent_across_recapture() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = commit_ledger_storage(tmp.path()).await;
        let commit = resolved_commit(
            "deadbeef00112233445566778899aabbccddeeff",
            &["Kb-Memory: abc123def456"],
        );
        let html = commit_capture_html("sess-commit-5", &[commit]);
        run_commit_ledger(&storage, tmp.path(), "sess-a.html", &html).await;
        run_commit_ledger(&storage, tmp.path(), "sess-b.html", &html).await;
        let rows = storage
            .memory_commits_for_memory("abc123def456".to_string(), 10)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "one FACT, one row — never one per capture");
        assert_eq!(
            rows[0].artifact_id,
            ArtifactId::from_path("sess-b.html").as_str(),
            "newest capture owns the row"
        );
    }

    /// A malformed `Kb-Memory:` value is dropped AND warned about — the
    /// trailer grammar lives in a shell hook this crate can't type-check, so
    /// drift must be greppable rather than a silent zero (the same posture
    /// the recall ledger's parse-gap warn takes).
    #[tokio::test]
    async fn memory_commit_ledger_hook_warns_on_a_malformed_trailer() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = commit_ledger_storage(tmp.path()).await;
        let html = commit_capture_html(
            "sess-commit-6",
            &[resolved_commit(
                "deadbeef00112233445566778899aabbccddeeff",
                &["Kb-Memory: not-a-hex-id"],
            )],
        );
        let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
        let buf_writer = buf.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || CaptureWriter(buf_writer.clone()))
            .with_ansi(false)
            .finish();
        let guard = tracing::subscriber::set_default(subscriber);
        run_commit_ledger(&storage, tmp.path(), "sess.html", &html).await;
        drop(guard);

        let logged = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(
            logged.contains("Kb-Memory trailer parse gap"),
            "expected the parse-gap warn, got: {logged}"
        );
        assert!(logged.contains("malformed=1"), "got: {logged}");
    }

    // --- CodeRefHook (DCB W1.A) -------------------------------------------

    /// A minimal harness for the code-ref hook: spawn storage, build a ctx
    /// over `html`, run the hook when its gate admits the doc, hand back both
    /// so the test can assert on the persisted rows. Callers must bind the
    /// returned `TempDir` guard (e.g. `let (storage, id, interested, _tmp) =
    /// …`) — dropping it tears down the on-disk store the `StorageHandle`
    /// still talks to.
    async fn code_ref_fixture(
        html: &str,
        kb_category: Option<&str>,
        content_hash: &str,
        now_unix: i64,
    ) -> (
        crate::storage::StorageHandle,
        ArtifactId,
        bool,
        tempfile::TempDir,
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let storage = crate::storage::StorageActor::spawn(
            tmp.path().join("lance"),
            tmp.path().join("index.db"),
            None,
        )
        .await
        .unwrap();
        let bus = EventBus::default();
        let kb_name = KbName::new("smoke").unwrap();
        let source_slug = SourceSlug::from_path(tmp.path());
        let path = tmp.path().join("plan.html");
        let artifact_id = ArtifactId::from_path("plan.html");
        let interested = {
            let ctx = EnrichCtx {
                kb_name: &kb_name,
                source_slug: &source_slug,
                storage: &storage,
                bus: &bus,
                quarantine_dir: tmp.path(),
                source_root: tmp.path(),
                path: &path,
                artifact_id: &artifact_id,
                rel_path: "plan.html",
                html,
                artifact_host_suffix: crate::iframe::DEFAULT_HOST_SUFFIX,
                kb_category,
                mtime_unix: 1_700_000_000,
                now_unix,
                seed_global: false,
                seed_linked_kbs: &[],
                content_hash,
                raw_source: html,
                versions_mode: crate::vcs::VersionsMode::Off,
                session_parse: None,
            };
            let interested = CodeRefHook.interested(&ctx);
            if interested {
                CodeRefHook.enrich(&ctx).await.unwrap();
            }
            interested
        };
        (storage, artifact_id, interested, tmp)
    }

    #[tokio::test]
    async fn code_ref_hook_skips_memory_sessions() {
        // A transcript is full of the literal `<code`, so only the CATEGORY
        // gate saves us from DOM-parsing a multi-MB capture per Stop.
        let html = "<body><p><code>app/models/order.rb:12</code></p></body>";
        let (_s, _id, interested, _tmp) =
            code_ref_fixture(html, Some("memory-session"), "deadbeef0001", 1_700_000_500).await;
        assert!(!interested, "memory-session transcripts are excluded");
        let (storage, id, interested, _tmp) =
            code_ref_fixture(html, Some("memory-fact"), "deadbeef0001", 1_700_000_500).await;
        assert!(interested, "an ordinary memory artifact is in scope");
        let doc = storage
            .code_refs_of(id.as_str().to_string())
            .await
            .unwrap()
            .expect("scanned");
        assert_eq!(doc.refs.len(), 1);
    }

    #[tokio::test]
    async fn code_ref_hook_gate_admits_issue_only_docs() {
        // No `<code` anywhere — only a GitHub issue link. The gate must still
        // admit it, else an issue-only doc is silently unscanned.
        let html = "<body><p>See \
                    <a href=\"https://github.com/acme/shopfront/issues/15351\">#15351</a>.</p></body>";
        let (storage, id, interested, _tmp) =
            code_ref_fixture(html, Some("feature"), "deadbeef0002", 1_700_000_500).await;
        assert!(interested);
        let doc = storage
            .code_refs_of(id.as_str().to_string())
            .await
            .unwrap()
            .expect("scanned");
        assert_eq!(doc.refs.len(), 1);
        assert_eq!(doc.refs[0].kind, "issue");
    }

    /// The gate's substring check must not be case-SENSITIVE while the parse
    /// regex behind it is `(?i)` — an all-caps-scheme, GitHub-cased href
    /// (`HTTPS://GitHub.com/…`) in a doc with no `<code` anywhere must still
    /// flip `interested()`, else it is silently never scanned.
    #[tokio::test]
    async fn code_ref_hook_gate_admits_uppercase_github_link() {
        let html = "<body><p>See \
                    <a href=\"HTTPS://GitHub.com/acme/shopfront/issues/15351\">#15351</a>.</p></body>";
        let (storage, id, interested, _tmp) =
            code_ref_fixture(html, Some("feature"), "deadbeef0004", 1_700_000_500).await;
        assert!(
            interested,
            "an uppercase-scheme GitHub.com link must gate in"
        );
        let doc = storage
            .code_refs_of(id.as_str().to_string())
            .await
            .unwrap()
            .expect("scanned");
        assert_eq!(doc.refs.len(), 1);
        assert_eq!(doc.refs[0].kind, "issue");
    }

    /// A doc that cites nothing still gets a header row — that is the whole
    /// reason `code_refs_docs` exists (zero-ref ≠ unscanned).
    #[tokio::test]
    async fn code_ref_hook_writes_a_header_for_a_zero_ref_doc() {
        let html = "<body><p><code>Product</code> and <code>EUR</code></p></body>";
        let (storage, id, _, _tmp) =
            code_ref_fixture(html, Some("feature"), "deadbeef0003", 1_700_000_500).await;
        let doc = storage
            .code_refs_of(id.as_str().to_string())
            .await
            .unwrap()
            .expect("scanned, found nothing");
        assert!(doc.refs.is_empty());
        assert_eq!(doc.header.ref_count, 0);
        assert_eq!(doc.header.doc_hash, "deadbeef0003");
    }

    /// DCB-W1.B.R — the honest `never_scanned` fix. A PROSE-ONLY doc (no
    /// `<code`, no GitHub link — fails the pre-gate that used to live in
    /// `interested()`) must still get a `code_refs_docs` header row: zero
    /// refs, `never_scanned == false` on the wire (`doc_out`'s `doc: Some`
    /// branch). Before this fix such a doc had NO row at all and
    /// `never_scanned` stayed `true` forever, unfixable by reindex.
    #[tokio::test]
    async fn code_ref_hook_prose_only_doc_is_scanned_not_never_scanned() {
        let html = "<body><h1>Design notes</h1><p>No code here, just prose \
                     about the roadmap and a plan for next quarter.</p></body>";
        let (storage, id, interested, _tmp) =
            code_ref_fixture(html, Some("feature"), "deadbeef0005", 1_700_000_500).await;
        assert!(interested, "interested() now admits every non-session doc");
        let doc = storage
            .code_refs_of(id.as_str().to_string())
            .await
            .unwrap()
            .expect("a header row must exist — never_scanned is false on the wire");
        assert_eq!(doc.header.ref_count, 0);
        assert!(doc.refs.is_empty());
    }

    /// The memory-session exclusion is still the ONE gate that leaves a doc
    /// with no header row at all (`never_scanned == true` on the wire).
    #[tokio::test]
    async fn code_ref_hook_memory_session_still_has_no_header_row() {
        let html = "<body><p><code>app/models/order.rb:12</code></p></body>";
        let (storage, id, interested, _tmp) =
            code_ref_fixture(html, Some("memory-session"), "deadbeef0006", 1_700_000_500).await;
        assert!(!interested);
        let doc = storage.code_refs_of(id.as_str().to_string()).await.unwrap();
        assert!(
            doc.is_none(),
            "memory-session docs must have NO code_refs_docs row — never_scanned stays true"
        );
    }

    /// The multi-MB-perf property: a doc that fails the pre-gate must skip
    /// `coderefs::extract`'s DOM walk entirely, not just discard its
    /// result. Proven indirectly (no mock seam on `extract`): the fixture
    /// html carries a `<meta name="kb-code-rev">` tag, which `extract` DOES
    /// read regardless of the `<code`/GitHub gate (see `extract`'s own
    /// unconditional `code_rev: doc.select(&sels.meta_code_rev)...`). If the
    /// walk ran, `code_rev` would be `Some`; because the pre-gate skips the
    /// walk entirely for a doc with no `<code`/GitHub signal, it stays
    /// `None` even though the meta tag is right there in the bytes.
    #[tokio::test]
    async fn code_ref_hook_prose_only_doc_never_reaches_extract() {
        let html = r#"<html><head>
            <meta name="kb-code-rev" content="shopfront@abc1234">
            </head><body><p>Just prose, no code signal at all.</p></body></html>"#;
        let (storage, id, _interested, _tmp) =
            code_ref_fixture(html, Some("feature"), "deadbeef0007", 1_700_000_500).await;
        let doc = storage
            .code_refs_of(id.as_str().to_string())
            .await
            .unwrap()
            .expect("scanned, zero refs");
        assert_eq!(
            doc.header.code_rev, None,
            "extract() must never run for a doc that fails the pre-gate — \
             its code_rev meta tag proves the DOM walk was skipped, not just discarded"
        );
    }

    /// R4 — `extracted_at` is `ctx.now_unix` (a literal in every test, never
    /// a real clock read) and only ADVANCES when the extraction changed.
    #[tokio::test]
    async fn code_ref_hook_extracted_at_is_ctx_now_unix() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = crate::storage::StorageActor::spawn(
            tmp.path().join("lance"),
            tmp.path().join("index.db"),
            None,
        )
        .await
        .unwrap();
        let bus = EventBus::default();
        let kb_name = KbName::new("smoke").unwrap();
        let source_slug = SourceSlug::from_path(tmp.path());
        let path = tmp.path().join("plan.html");
        let artifact_id = ArtifactId::from_path("plan.html");
        let html_a = "<body><p><code>app/models/order.rb:12</code></p></body>";
        let html_b = "<body><p><code>app/models/order.rb:99</code></p></body>";

        let build = |html: &'static str, hash: &'static str, now: i64| EnrichCtx {
            kb_name: &kb_name,
            source_slug: &source_slug,
            storage: &storage,
            bus: &bus,
            quarantine_dir: tmp.path(),
            source_root: tmp.path(),
            path: &path,
            artifact_id: &artifact_id,
            rel_path: "plan.html",
            html,
            artifact_host_suffix: crate::iframe::DEFAULT_HOST_SUFFIX,
            kb_category: Some("feature"),
            mtime_unix: 1_700_000_000,
            now_unix: now,
            seed_global: false,
            seed_linked_kbs: &[],
            content_hash: hash,
            raw_source: html,
            versions_mode: crate::vcs::VersionsMode::Off,
            session_parse: None,
        };

        CodeRefHook
            .enrich(&build(html_a, "deadbeef0001", 1_700_000_000))
            .await
            .unwrap();
        // Same bytes, later clock (a `kb reindex --force` pass): unchanged, so
        // `extracted_at` must stay pinned to the FIRST write.
        CodeRefHook
            .enrich(&build(html_a, "deadbeef0001", 1_700_000_500))
            .await
            .unwrap();
        let doc = storage
            .code_refs_of(artifact_id.as_str().to_string())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(doc.header.extracted_at, 1_700_000_000);

        // A CHANGED extraction advances it to that call's `now_unix`.
        CodeRefHook
            .enrich(&build(html_b, "deadbeef0002", 1_700_000_500))
            .await
            .unwrap();
        let doc = storage
            .code_refs_of(artifact_id.as_str().to_string())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(doc.header.extracted_at, 1_700_000_500);
        assert_eq!(doc.refs[0].line_start, Some(99));
    }

    /// The group triple is denormalised onto every ref, and an ungrouped ref
    /// carries all three as NULL — never a synthetic `""` group (R9).
    #[test]
    fn to_code_ref_rows_denormalises_groups_and_nulls_ungrouped() {
        let e = crate::coderefs::extract(
            "<body><p><code>config/importmap.rb</code></p>\
             <h2>Work items</h2><p><code>orders/indexer.rb</code></p></body>",
        );
        let rows = to_code_ref_rows(&e);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].group_key, None);
        assert_eq!(rows[0].group_label, None);
        assert_eq!(rows[0].group_anchor, None);
        assert_eq!(rows[1].group_key.as_deref(), Some("kb-h-work-items"));
        assert_eq!(rows[1].group_label.as_deref(), Some("Work items"));
        assert_eq!(rows[1].group_anchor.as_deref(), Some("kb-h-work-items"));
        assert_eq!(rows[1].context_tokens, "indexer");
    }
}
