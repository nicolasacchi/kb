//! Indexer pipeline — wires the watcher → parser → storage actor → events
//! bus. Subscribes to `watch.*` envelopes on the bus; for each, reads the
//! file, parses it, builds a `Doc`, and upserts via the storage actor.
//!
//! Discipline (discussion topic 10): skip + log + continue on parse failure;
//! retry on next event; quarantine after 3 consecutive failures with the
//! same content hash.
//!
//! Run lifecycle (topic 11 §B.3): a "run" wraps a logical batch — for v0.0.1
//! that means each `watch.*` event is its own one-file run, since there's
//! no external `POST .../reindex` API yet (deferred to v0.0.1 phase 15
//! where kb-server adds the route). The lifecycle is still useful: emits
//! `index.start` / `index.file` / `index.complete` per file so the SSE
//! consumer (TUI / SPA) sees granular progress.

use crate::embed::Embedder;
use crate::events::EventBus;
use crate::ids::{ArtifactId, RunId, SourceSlug};
use crate::parser;
use crate::storage::schema::Doc;
use crate::storage::StorageHandle;
use crate::types::{ChangeKind, KbName};
use crate::watcher::is_skipped;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

/// Shared per-kb embedder handle. Single-threaded fastembed model held
/// behind a sync Mutex (fastembed::TextEmbedding::embed is `&mut self`).
/// `Option` so v0.0.1-style indexing without embeddings still works for
/// tests + early integration runs.
pub type EmbedderHandle = Option<Arc<Mutex<Embedder>>>;

/// Quarantine threshold per discussion topic 10 (skip+log+continue;
/// quarantine after 3 consecutive failures with the same content hash).
pub const QUARANTINE_THRESHOLD: u32 = 3;

/// Cap on chunk rows embedded per document (SQ5 chunk pass, below).
/// 2026-08-21 ci-host incident, defect 2: with the embed-input cap already in
/// place (ab6a05e9), a single oversized doc no longer OOMs the embedder
/// per call — but on a chunked corpus the SAME 5.09M-word doc still gets
/// chunked at the default 220-word stride (`DEFAULT_CHUNK_WORDS` minus
/// `DEFAULT_OVERLAP_WORDS`) into ~23,137 chunks, ~2,893 mini-batches at
/// `INDEX_EMBED_MINI_BATCH=8`, minutes of embedder IPC for one doc — a
/// cost the earlier OOM crash simply masked. 512 chunks ≈ the first
/// ~115K words at that stride, far beyond what ranking needs (chunk
/// max-pooling already saturates search relevance long before then). The
/// FIRST `MAX_CHUNKS_PER_DOC` chunks are kept — chunk-0 (title/headings)
/// included, since chunks are produced in reading order starting at idx
/// 0 — deterministic, honest: the doc is still fully searchable via its
/// doc-level embedding + BM25 over the complete body; this cap only
/// bounds the per-doc chunk-embed cost.
pub const MAX_CHUNKS_PER_DOC: usize = 512;

/// Outcome of a [`walk_emit_modify`] pass — the envelope count plus the
/// canonical set of on-disk paths walked. The reconciler uses the set
/// to diff against `storage.list_docs()` and emit synthetic
/// `watch.delete` envelopes for rows whose files vanished.
pub struct WalkOutcome {
    pub emitted: usize,
    pub seen_paths: HashSet<PathBuf>,
}

/// Bound on the per-kb ingest channel (G7). The indexer drains one item at a
/// time (each ~170 ms embed), so this is how far a bulk producer (reconcile /
/// reindex walk) may run AHEAD of the indexer before back-pressure blocks it —
/// the property the old shared broadcast bus could not provide (it dropped
/// instead). Sized to the storage-actor channel for symmetry.
pub const INGEST_QUEUE_CAPACITY: usize = 1024;

/// GC-B7 — max `WatchWork` items opportunistically drained (via `try_recv`,
/// never blocking) past the first `rx.recv().await` into one storage
/// `upsert_docs` batch, so a live-watcher burst or a cold bulk import commits
/// ONE Lance fragment + manifest version for the whole group instead of one
/// per file. The 2026-07-11 20k-doc scale test traced the ingest cliff
/// (1.73 -> 0.36 docs/s by ~1,500 docs) directly to the one-fragment-
/// per-doc-upsert pattern. 32 is a modest cap: large enough to collapse most
/// of a reconcile/reindex backlog's per-file commits, small enough that a
/// single flush's `merge_insert` (and its all-or-nothing bisect retry on
/// failure, see `flush_prepared_batch`) never holds up the actor for long.
const INGEST_BATCH_MAX_DOCS: usize = 32;

/// Byte-budget companion to `INGEST_BATCH_MAX_DOCS`: stop growing a batch
/// once its accumulated body+html+raw bytes would exceed ~8 MB, so a
/// handful of oversized artifacts (a multi-MB session transcript) can't
/// blow up one flush's peak memory the way an unconditional 32-doc drain
/// could. 8 MB comfortably covers 32 average corpus artifacts (typically a
/// few KB to a few hundred KB each per the research corpus) while still
/// bounding the pathological case.
const INGEST_BATCH_MAX_BYTES: usize = 8 * 1024 * 1024;

/// One unit of indexer ingest work, delivered over the per-kb back-pressured
/// mpsc channel (G7). Replaces the broadcast `watch.*` envelope the indexer
/// used to filter off the shared observability bus.
#[derive(Debug, Clone)]
pub struct WatchWork {
    pub kind: WatchKind,
    pub path: PathBuf,
    /// Bypass the content-hash dedup gate (operator reindex).
    pub force: bool,
    /// X1 — for a `Deleted` unit only: route `process_delete` through
    /// [`crate::cascade::CascadeMode::KeepUserData`] instead of `Full`. Set
    /// ONLY by the reconciler when a stored row's file is still on disk but its
    /// extension left the resolved map (an X1 map SHRINK): the row must leave
    /// the index (the TRAP fix) but the `.review` comment sidecar + reading
    /// `history` must survive, because the file is still there and re-widening
    /// the map has to bring the comments back. `false` for every genuine
    /// delete (file vanished → the row AND its user data go).
    pub keep_user_data: bool,
}

/// The filesystem change a [`WatchWork`] represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchKind {
    Created,
    Modified,
    Deleted,
}

impl WatchKind {
    /// The legacy bus event type, kept for the observability mirror so the
    /// TUI watcher heatmap + SPA event log keep seeing `watch.*` frames.
    fn bus_type(self) -> &'static str {
        match self {
            WatchKind::Created => "watch.create",
            WatchKind::Modified => "watch.modify",
            WatchKind::Deleted => "watch.delete",
        }
    }
}

/// The authoritative ingest path for one kb (G7). Every producer (live
/// watcher, reconciler walk + delete pass, operator reindex, route nudges)
/// pushes through a sink:
///
/// - **Authoritative:** the work goes onto the bounded mpsc the indexer
///   drains. `send`/`blocking_send` BLOCK when the channel is full — real
///   back-pressure, so a burst larger than the queue can't silently drop
///   `watch.*` the way the old broadcast bus did (deep-review HIGH).
/// - **Observability mirror:** the same event is emitted to the `EventBus`
///   as a `watch.*` frame. The indexer reads ONLY the mpsc and never the bus,
///   so the mirror is never re-consumed (no double-processing) — it exists
///   purely for the TUI heatmap / SPA event log / webhook subscribers.
#[derive(Clone)]
pub struct IngestSink {
    tx: mpsc::Sender<WatchWork>,
    bus: Arc<EventBus>,
    kb: KbName,
    /// X1 — the resolved indexable-extension map for this kb. THE carrier for
    /// every ingest-gate seam that already holds a `&IngestSink`: the walk
    /// (`walk_send_work`→`walk_core`), the reconciler delete pass, and the
    /// watcher (`initial_walk` / `emit_for_paths`). Defaults to
    /// [`crate::extmap::ExtensionMap::default`] (pre-X1 set) so the ~dozen
    /// existing `IngestSink::new` call sites need no change; production
    /// (`bring_up_kb`) installs the resolved map via [`Self::with_extensions`].
    extensions: crate::extmap::ExtensionMap,
    /// X2 — the kb's shared runtime enforcement gate (excluded rel paths +
    /// the D6 paused flag). Rides the sink for the same reason `extensions`
    /// does: every producer-side seam (walk, watcher, reconcile delete pass)
    /// already holds a sink. Cloning the sink shares the SAME gate (an `Arc`
    /// inside), so a runtime exclusion/pause is visible to every seam at
    /// once. Defaults to an empty gate; production installs the loaded one
    /// via [`Self::with_gate`].
    gate: crate::exclusions::IngestGate,
}

impl IngestSink {
    pub fn new(tx: mpsc::Sender<WatchWork>, bus: Arc<EventBus>, kb: KbName) -> Self {
        Self {
            tx,
            bus,
            kb,
            extensions: crate::extmap::ExtensionMap::default(),
            gate: crate::exclusions::IngestGate::default(),
        }
    }

    /// X1 — install the resolved per-kb extension map. Builder-style so the
    /// default-map `new` call sites stay untouched.
    pub fn with_extensions(mut self, extensions: crate::extmap::ExtensionMap) -> Self {
        self.extensions = extensions;
        self
    }

    /// X2 — install the kb's shared ingest gate (exclusions + paused).
    pub fn with_gate(mut self, gate: crate::exclusions::IngestGate) -> Self {
        self.gate = gate;
        self
    }

    /// The observability EventBus this sink mirrors to (for non-ingest events
    /// a producer also emits, e.g. `watcher.lagged`).
    pub fn bus(&self) -> &Arc<EventBus> {
        &self.bus
    }

    /// The kb this sink ingests for (logging).
    pub fn kb(&self) -> &KbName {
        &self.kb
    }

    /// X1 — the resolved extension map. Read by the walk gate + the
    /// reconciler delete pass + the watcher gates, all of which hold a sink.
    pub fn extensions(&self) -> &crate::extmap::ExtensionMap {
        &self.extensions
    }

    /// X2 — the shared ingest gate. Read by the same seams as `extensions`;
    /// written by the exclusion ops + the pause/resume route.
    pub fn gate(&self) -> &crate::exclusions::IngestGate {
        &self.gate
    }

    fn mirror(&self, kind: WatchKind, path: &Path, force: bool) {
        let mut payload = json!({
            "kb": self.kb.as_str(),
            "path": path.to_string_lossy(),
        });
        if force {
            payload["force"] = serde_json::Value::Bool(true);
        }
        self.bus.emit(kind.bus_type(), payload);
    }

    /// Async push (route handlers, the reconciler's delete pass). Mirrors to
    /// the bus, then awaits channel space (back-pressure). A closed channel
    /// (indexer gone) is ignored — the daemon is tearing down.
    pub async fn send(&self, kind: WatchKind, path: PathBuf, force: bool) {
        self.mirror(kind, &path, force);
        let _ = self
            .tx
            .send(WatchWork {
                kind,
                path,
                force,
                keep_user_data: false,
            })
            .await;
    }

    /// X1 — async push of an EXCLUSION-shaped delete: a still-present file
    /// whose extension left the resolved map (a map shrink). Same observability
    /// mirror as a normal `Deleted`, but flags `keep_user_data` so the cascade
    /// drops the index row while KEEPING the `.review` sidecar + reading
    /// history (the file is still on disk; re-widening the map restores it).
    /// The reconciler's delete pass is the only caller.
    pub async fn send_unmapped_delete(&self, path: PathBuf) {
        self.mirror(WatchKind::Deleted, &path, false);
        let _ = self
            .tx
            .send(WatchWork {
                kind: WatchKind::Deleted,
                path,
                force: false,
                keep_user_data: true,
            })
            .await;
    }

    /// Sync push for blocking contexts — the watcher's std::thread drain and
    /// the `spawn_blocking` reconcile/reindex walk. `blocking_send` provides
    /// the same back-pressure from a non-async thread. MUST NOT be called on a
    /// tokio runtime worker (it would panic); both call sites are blocking
    /// threads.
    pub fn blocking_send(&self, kind: WatchKind, path: PathBuf, force: bool) {
        self.mirror(kind, &path, force);
        let _ = self.tx.blocking_send(WatchWork {
            kind,
            path,
            force,
            keep_user_data: false,
        });
    }
}

/// Bridge a broadcast `watch.*` stream into a [`WatchWork`] mpsc — used ONLY
/// by the back-compat [`run`] wrapper so the pre-G7 test/integration drivers
/// that `bus.emit("watch.create", …)` keep working. Production never uses this
/// (producers push to the sink directly); the indexer's `run_with_ingest`
/// reads the mpsc regardless of source.
async fn bridge_watch_to_ingest(
    mut rx: tokio::sync::broadcast::Receiver<crate::types::Envelope>,
    tx: mpsc::Sender<WatchWork>,
    kb_name: KbName,
) {
    use tokio::sync::broadcast::error::RecvError;
    loop {
        match rx.recv().await {
            Ok(env) => {
                if env.payload["kb"].as_str() != Some(kb_name.as_str()) {
                    continue;
                }
                let Some(path) = env.payload["path"].as_str() else {
                    continue;
                };
                let kind = match env.type_.as_str() {
                    "watch.create" => WatchKind::Created,
                    "watch.modify" => WatchKind::Modified,
                    "watch.delete" => WatchKind::Deleted,
                    _ => continue,
                };
                let force = env
                    .payload
                    .get("force")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if tx
                    .send(WatchWork {
                        kind,
                        path: PathBuf::from(path),
                        force,
                        // The broadcast bridge is test-only and never carries
                        // an exclusion-shaped delete (only the reconciler
                        // produces those, via the sink directly).
                        keep_user_data: false,
                    })
                    .await
                    .is_err()
                {
                    break; // indexer gone
                }
            }
            Err(RecvError::Closed) => break,
            // Best-effort bridge: tests don't overflow; a real overflow on the
            // bus is the reconciler's job to recover, not this shim's.
            Err(RecvError::Lagged(_)) => continue,
        }
    }
}

/// True for a Markdown source (`.md` / `.markdown`). Markdown artifacts are
/// rendered to HTML at serve time (see [`crate::markdown`]); the indexer
/// renders them once here to extract fields + link edges.
pub fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("md") || s.eq_ignore_ascii_case("markdown"))
}

/// True for the BUILT-IN default artifact set: HTML or Markdown
/// (`html`/`htm`/`md`/`markdown`). X1 note: the LIVE ingest gate is now the
/// per-kb [`crate::extmap::ExtensionMap`] (carried on the [`IngestSink`]), not
/// this free fn — a kb can map extra extensions (e.g. `.txt` → Markdown) or
/// narrow the set. This predicate is retained as the canonical default set
/// (it is exactly [`crate::extmap::ExtensionMap::default`]) for tests and any
/// caller that wants the default without a resolved map.
pub fn is_indexable(path: &Path) -> bool {
    crate::extmap::ExtensionMap::default().is_indexable(path)
}

/// Walk `source_root` and emit a `watch.modify` envelope for every
/// indexable (`.html`/`.htm`/`.md`) file found, skipping anything matched by
/// `skip_patterns` (same grammar as the watcher's live filter; see
/// [`crate::watcher::path_matches_skip_pattern`]).
///
/// Returns the count of envelopes plus the set of seen paths.
///
/// Synchronous + uses `walkdir`. Callers running inside a tokio
/// runtime should wrap the call in `tokio::task::spawn_blocking` to
/// keep the executor responsive on large corpora.
///
/// Used by:
/// - `routes/reindex.rs` — operator-triggered full rescan. Passes
///   `force = true` so the indexer bypasses the byte-identical dedup
///   gate (the reindex is explicit; the operator wants the full
///   pipeline incl. edge resolution to re-run).
/// - [`reconcile`] — periodic safety net that catches files notify
///   missed (inotify queue overflow, NFS mounts, dropped broadcast lag,
///   dead drain thread). Passes `force = false` so byte-identical
///   files short-circuit cheaply and the pass is near-free when
///   nothing changed.
///
/// `skip_patterns` MUST be threaded from the kb's configured
/// `[kb.foo] skip_patterns` (kb_section.skip_patterns). Pre-v0.7.x the
/// reconciler omitted this, silently re-indexing user-excluded files
/// every interval.
pub fn walk_emit_modify(
    bus: &EventBus,
    kb_name: &KbName,
    source_root: &Path,
    skip_patterns: &[String],
    force: bool,
    known_mtimes: Option<&HashMap<PathBuf, i64>>,
) -> WalkOutcome {
    // X1 — the legacy bus-emit walk is test-only (production uses
    // `walk_send_work` via reconcile / reindex, which carries the resolved
    // map on the sink). It has no sink to read a per-kb map from, so it
    // always uses the built-in default set — enough for the default-behaviour
    // tests that exercise it. Same story for the X2 gate: no sink, so an
    // empty default gate (no exclusions, unpaused).
    let extensions = crate::extmap::ExtensionMap::default();
    let gate = crate::exclusions::IngestGate::default();
    walk_core(
        source_root,
        skip_patterns,
        force,
        known_mtimes,
        &extensions,
        &gate,
        |p| {
            let mut payload = json!({
                "kb": kb_name.as_str(),
                "path": p.to_string_lossy(),
            });
            if force {
                payload["force"] = serde_json::Value::Bool(true);
            }
            bus.emit("watch.modify", payload);
        },
    )
}

/// G7 back-pressured twin of [`walk_emit_modify`]: pushes each changed file
/// onto the per-kb ingest channel via the sink (which mirrors to the bus too)
/// instead of fire-and-forget bus emits. Runs in `spawn_blocking`, so the
/// sink's `blocking_send` blocks the walk when the indexer falls behind —
/// the bulk-walk back-pressure that stops a >queue burst from dropping work.
/// The reconciler and operator `reindex` use this; the same G5 mtime dedup
/// applies, so a no-op reconcile still pushes ~nothing.
pub fn walk_send_work(
    sink: &IngestSink,
    source_root: &Path,
    skip_patterns: &[String],
    force: bool,
    known_mtimes: Option<&HashMap<PathBuf, i64>>,
) -> WalkOutcome {
    // X1/X2 — the sink carries the kb's resolved extension map AND the shared
    // ingest gate; the walk reads both from there (no extra param on the
    // ~half-dozen call sites).
    walk_core(
        source_root,
        skip_patterns,
        force,
        known_mtimes,
        sink.extensions(),
        sink.gate(),
        |p| {
            sink.blocking_send(WatchKind::Modified, p.to_path_buf(), force);
        },
    )
}

/// Shared walk body for [`walk_emit_modify`] / [`walk_send_work`]. Walks
/// `source_root`, applies `is_indexable` + `skip_patterns` + the G5
/// producer-side mtime dedup, records EVERY surviving file in `seen_paths`
/// (existence — what the reconciler's delete pass diffs against), and invokes
/// `emit(path)` once per file that should be (re)indexed. `emitted` counts the
/// `emit` calls; `seen_paths` counts all files on disk.
fn walk_core(
    source_root: &Path,
    skip_patterns: &[String],
    force: bool,
    known_mtimes: Option<&HashMap<PathBuf, i64>>,
    // X1 — the resolved extension map is THE ingest gate. A file whose final
    // extension isn't mapped is skipped here (and so is absent from
    // `seen_paths`, which is what makes a map-SHRINK route a de-mapped-but-
    // existing file to the reconciler's delete pass — see `reconcile`).
    extensions: &crate::extmap::ExtensionMap,
    // X2 — the shared runtime gate (excluded rel paths + D6 paused). Excluded
    // files are treated exactly like de-mapped ones (skipped BEFORE
    // `seen_paths`, so the reconciler's delete pass produces the explicit
    // KeepUserData delete decision); paused suppresses EMISSION only, after
    // `seen_paths` (existence must keep being tracked or the delete pass
    // would misread paused as vanished).
    gate: &crate::exclusions::IngestGate,
    mut emit: impl FnMut(&Path),
) -> WalkOutcome {
    let paused = gate.paused();
    let mut emitted = 0usize;
    let mut seen_paths: HashSet<PathBuf> = HashSet::new();
    for entry in walkdir::WalkDir::new(source_root)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let p = entry.path();
        if !extensions.is_indexable(p) {
            continue;
        }
        // Skip-patterns parity with the live watcher (`watcher::is_skipped`),
        // so a user who configured `skip_patterns = ["templates/**"]`
        // doesn't see those files resurrected by the reconciler.
        if is_skipped(p, source_root, skip_patterns) {
            continue;
        }
        // X2 — excluded files are invisible to the walk: not emitted AND
        // absent from `seen_paths`, so the reconciler's delete pass re-stats
        // any stored row for them and routes it through the explicit
        // KeepUserData delete (THE TRAP fix — absence alone deletes nothing).
        if gate.is_excluded(p, source_root) {
            continue;
        }
        let p_buf = p.to_path_buf();
        // Existence is tracked for the delete pass REGARDLESS of whether we
        // re-emit below — a byte-identical, unchanged file still EXISTS, so
        // omitting it from `seen_paths` would make the reconciler emit a
        // bogus `watch.delete` for it.
        seen_paths.insert(p_buf.clone());

        // X2/D6 — a paused source emits nothing for ingest (reconcile,
        // operator reindex, overflow rescan alike; resume + the next
        // reconcile tick catch the source back up). Existence tracking above
        // is deliberately unaffected.
        if paused {
            continue;
        }

        // G5 — producer-side dedup. On the safety-net pass (`!force`), skip
        // emitting for a file whose on-disk mtime equals the mtime stored at
        // last index. The indexer's content-hash PRE-gate (G4) already drops
        // such no-ops silently, but it still had to receive one item per file
        // per reconcile. Skipping here makes a truly-unchanged reconcile pass
        // push ~nothing. Cheap: stat-only (no read), reusing walkdir's cached
        // metadata. A touched-but-unchanged file (mtime bumped, bytes
        // identical) still emits and is caught downstream by the content-hash
        // gate, so correctness is unchanged; a map MISS (unknown path, or
        // path-representation mismatch with stored rows) degrades safely to
        // "emit anyway". `force` (operator reindex) always emits.
        if !force {
            if let Some(known) = known_mtimes {
                if let Some(&stored) = known.get(&p_buf) {
                    let disk_mtime = entry
                        .metadata()
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64);
                    if disk_mtime == Some(stored) {
                        continue; // unchanged since last index — don't re-emit
                    }
                }
            }
        }

        emit(p);
        emitted += 1;
    }
    WalkOutcome {
        emitted,
        seen_paths,
    }
}

/// R1 — summary of a single reconcile pass. Recorded by the
/// reconcile-loop in kb-server so `GET /api/kb/{kb}/stats` can surface
/// staleness ("last reconcile completed at T; emitted N watch.modify
/// for files on disk; detected D missing rows"). All four fields are
/// observed locally to `reconcile()` — they do NOT count downstream
/// indexer outcomes (parse failures, hash-dedup skips). For those, the
/// caller pairs the summary with `KbStats::open_errors` /
/// `KbStats::doc_count`.
#[derive(Debug, Clone, Copy)]
pub struct ReconcileSummary {
    /// Wall-clock unix seconds when the pass completed.
    pub completed_at: i64,
    /// Indexable files seen on disk this pass. NOTE: since G5 this is the
    /// count of files SEEN (existence — what the delete pass checks against),
    /// NOT the count that re-emitted a `watch.modify`; the producer-side
    /// dedup skips emitting for unchanged-mtime files, so emits ≤ files_walked.
    pub files_walked: u64,
    /// Synthetic `watch.delete` events emitted for storage rows whose
    /// underlying file no longer exists.
    pub deletes_emitted: u64,
    /// Wall-clock duration of the pass in milliseconds.
    pub duration_ms: u64,
}

/// Full reconcile pass: walks the filesystem (emitting `watch.modify`
/// for every non-skipped HTML file), then queries storage for known
/// rows under `source_root` and emits a synthetic `watch.delete` for
/// each row whose file no longer exists on disk.
///
/// This is the W1 safety net: the live watcher misses delete events
/// (inotify queue overflow being the canonical case W1 was meant to
/// fix), and `walk_emit_modify` alone only re-asserts existence — it
/// leaves phantom rows in lance for any delete the watcher missed.
/// Per CLAUDE.md's W1 invariant the reconciler is the "self-healing"
/// layer; without delete propagation it isn't.
///
/// The walk runs in `spawn_blocking` for the same reason as
/// `walk_emit_modify`; the storage query is awaited on the caller's
/// runtime.
///
/// R1: returns a `ReconcileSummary` instead of a bare tuple so callers
/// (the kb-server reconcile loop) can surface staleness on `kb status`.
pub async fn reconcile(
    sink: IngestSink,
    storage: StorageHandle,
    source_root: PathBuf,
    skip_patterns: Vec<String>,
) -> ReconcileSummary {
    let started = std::time::Instant::now();
    let kb_name = sink.kb().clone();

    // Canonicalise the source root once so the storage paths
    // (stored canonical) and the walked paths (also canonical via
    // walkdir's resolution) compare like-for-like. M5: async — runs
    // once per reconcile pass, but on NFS even one sync canonicalize
    // can block the runtime for seconds.
    let canonical_root = tokio::fs::canonicalize(&source_root)
        .await
        .unwrap_or_else(|_| source_root.clone());

    // Fetch known rows UP FRONT: their mtimes drive the G5 producer-side
    // dedup in the walk, and the SAME list drives the delete pass below. On
    // a storage error we can neither dedup nor delete — fall back to a full
    // emit (no mtime map, so the safety net still re-asserts existence) and
    // skip the delete pass.
    //
    // Perf: reconcile only consumes each row's path (delete pass) and
    // mtime (dedup map), so it pulls the narrow `(path, mtime_unix)`
    // projection rather than the full `list_docs` slim scan (title/tags/
    // body-excerpt/atlas + an in-memory sort it never uses). This trims
    // the per-tick occupancy of the single-writer storage actor, which the
    // periodic reconcile loop otherwise contends against live search reads.
    let docs = match storage.list_reconcile_rows().await {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(
                kb = %kb_name,
                error = %e,
                "reconcile: list_reconcile_rows failed; full-emit fallback, skipping delete pass",
            );
            let sink_w = sink.clone();
            let root_w = canonical_root.clone();
            let skips_w = skip_patterns.clone();
            let walk = tokio::task::spawn_blocking(move || {
                walk_send_work(&sink_w, &root_w, &skips_w, false, None)
            })
            .await
            .unwrap_or(WalkOutcome {
                emitted: 0,
                seen_paths: HashSet::new(),
            });
            return ReconcileSummary {
                completed_at: now_unix(),
                files_walked: walk.seen_paths.len() as u64,
                deletes_emitted: 0,
                duration_ms: started.elapsed().as_millis() as u64,
            };
        }
    };

    // Build path -> stored-mtime for rows under THIS source root (same
    // scoping as the delete pass). Keyed identically to the delete-pass
    // lookup (`PathBuf::from(&doc.path)`), so any path-representation
    // mismatch degrades to a map MISS → "emit anyway", never a wrong skip.
    let mut known_mtimes: HashMap<PathBuf, i64> = HashMap::new();
    for (_id, doc_path, mtime) in &docs {
        if let Some(mt) = mtime {
            let path = PathBuf::from(doc_path);
            if path.starts_with(&canonical_root) {
                known_mtimes.insert(path, *mt);
            }
        }
    }

    let sink_for_walk = sink.clone();
    // Walk the CANONICAL root so emitted/seen paths match the canonical
    // `Doc.path` we store (and the canonical_root prefix check below). With a
    // symlinked source root this is what makes vanished-file deletes fire.
    let root_for_walk = canonical_root.clone();
    let skips_for_walk = skip_patterns.clone();
    let walk = tokio::task::spawn_blocking(move || {
        walk_send_work(
            &sink_for_walk,
            &root_for_walk,
            &skips_for_walk,
            false,
            Some(&known_mtimes),
        )
    })
    .await
    .unwrap_or(WalkOutcome {
        emitted: 0,
        seen_paths: HashSet::new(),
    });

    // R2 — snapshot the live lance doc-id set for the orphan sweep BEFORE the
    // delete loop consumes `docs` (reconcile already holds the full list, so
    // the sweep costs no extra lance scan).
    let live_ids: HashSet<String> = docs.iter().map(|(id, _, _)| id.clone()).collect();

    let mut deletes = 0usize;
    for (_id, doc_path, _mtime) in docs {
        let path = PathBuf::from(&doc_path);
        // Only reconcile rows whose path lives under THIS kb's source
        // root — multi-kb daemons share the storage actor by kb_name,
        // but `list_reconcile_rows` returns every doc for the kb regardless
        // of source. Comparing under the source root avoids deleting rows
        // owned by sibling sources or by a previous source_path that
        // was rebound.
        if !path.starts_with(&canonical_root) {
            continue;
        }
        if walk.seen_paths.contains(&path) {
            continue;
        }
        // The file is not in `seen_paths` (which only collects files that
        // PASSED `is_indexable` + the X2 exclusion gate in the walk). Three
        // reasons a stored row lands here: the file vanished, OR it still
        // exists but its extension is no longer in the resolved map (an X1
        // map SHRINK), OR it is EXCLUDED (X2 — typically excluded while the
        // daemon was down; the live exclude op cascades immediately). All
        // must drop the index row — but the pre-X1 guard
        // `if path.try_exists() { continue }` kept EVERY still-existing file,
        // so a de-mapped file's row/index lingered forever (THE TRAP).
        // Re-stat and decide:
        //   - exists + mapped + not excluded → transient `walkdir` miss
        //     (permission race); keep the row (under-delete, as before).
        //   - can't stat (Err)       → keep the row (the old `unwrap_or(true)`).
        //   - exists + de-mapped OR excluded → EXCLUSION-shaped: drop the row
        //     via the KeepUserData cascade so the `.review` comments + reading
        //     history SURVIVE (the file is still on disk; re-widening the map /
        //     re-including must bring them back — Full here would permanently
        //     destroy non-re-derivable comments).
        //   - vanished               → a real delete: the Full cascade reaps
        //     everything, user data included (the file is gone for good).
        // Both delete arms route the synthetic delete through the sink: it
        // mirrors `watch.delete` to the bus (observability + the reconcile-
        // delete test) AND pushes onto the back-pressured ingest channel for
        // `process_delete`, which runs the chosen cascade.
        match path.try_exists() {
            Ok(true)
                if sink.extensions().is_indexable(&path)
                    && !sink.gate().is_excluded(&path, &canonical_root) =>
            {
                continue
            }
            Err(_) => continue,
            Ok(true) => {
                // de-mapped or excluded but still on disk → KeepUserData
                sink.send_unmapped_delete(path).await;
                deletes += 1;
            }
            Ok(false) => {
                // vanished → Full cascade
                sink.send(WatchKind::Deleted, path, false).await;
                deletes += 1;
            }
        }
    }

    // R2 — orphan backstop. After the existing delete pass, prune sqlite
    // dependents (edges / corkboard / pinned_memories / history +
    // reading_sections) that reference ids no longer in lance — reclaiming
    // leaks from pre-v0.24 deletes that never cascaded. `list_entries` is
    // deliberately NOT swept: an entry pointing at a deleted artifact is an
    // intentional tombstone. X2 — the exempt set is the gate's excluded
    // rel-path snapshot, so an excluded doc's KEPT history rows (the
    // KeepUserData cascade left them for re-include) are never swept as
    // orphans. Cheap when clean (one DISTINCT scan + a set diff per table);
    // logs only when it reaps.
    let exempt = sink.gate().excluded_snapshot();
    match crate::cascade::sweep_orphans(&storage, live_ids, &exempt).await {
        Ok(report) if report.total_rows > 0 => {
            tracing::info!(
                kb = %kb_name,
                rows = report.total_rows,
                "reconcile: swept orphaned sqlite dependents",
            );
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(kb = %kb_name, error = %e, "reconcile: orphan sweep failed");
        }
    }

    ReconcileSummary {
        completed_at: now_unix(),
        files_walked: walk.seen_paths.len() as u64,
        deletes_emitted: deletes as u64,
        duration_ms: started.elapsed().as_millis() as u64,
    }
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Build the indexer for one kb. Returns a future that runs until the bus
/// closes (i.e. `EventBus` is dropped).
///
/// The caller MUST subscribe to the bus and pass the receiver in — this
/// avoids the start-then-subscribe race where events fire before the
/// indexer task has a chance to subscribe.
/// `review_dir` (v0.3 G2): directory holding kb-comments/1 review
/// files. When `Some`, the post-upsert hook re-resolves each open
/// comment's anchor against the freshly-parsed HTML and emits
/// `comment.anchor_stale` for any that no longer bind. `None` disables
/// the check (used by tests that don't care about comments).
/// Back-compat wrapper around [`run_with_ingest`] (G7). Bridges a broadcast
/// `watch.*` receiver — the pre-G7 ingest source, still used by tests and any
/// broadcast-only driver — into the per-kb [`WatchWork`] mpsc the indexer now
/// consumes. Production (`bring_up_kb`) calls `run_with_ingest` directly, with
/// producers pushing to the [`IngestSink`]; only this shim reads ingest off
/// the bus.
///
/// `reconcile_secs` / `skip_patterns` are vestigial since the G3 Lagged→
/// re-walk escalation was removed (the bounded mpsc back-pressures instead of
/// dropping, so there's nothing to recover from). They're retained so the
/// pre-G7 call sites compile unchanged.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    kb_name: KbName,
    source_slug: SourceSlug,
    storage: StorageHandle,
    bus: Arc<EventBus>,
    quarantine_dir: PathBuf,
    rx: tokio::sync::broadcast::Receiver<crate::types::Envelope>,
    embedder: EmbedderHandle,
    review_dir: Option<PathBuf>,
    artifact_host_suffix: String,
    versions_mode: crate::vcs::VersionsMode,
    _reconcile_secs: u64,
    _skip_patterns: Vec<String>,
) {
    let (tx, work_rx) = mpsc::channel(INGEST_QUEUE_CAPACITY);
    let bridge = tokio::spawn(bridge_watch_to_ingest(rx, tx, kb_name.clone()));
    run_with_ingest(
        kb_name,
        source_slug,
        storage,
        bus,
        quarantine_dir,
        work_rx,
        embedder,
        review_dir,
        // The broadcast-bridge shim is test-only; no comment lock to share.
        None,
        artifact_host_suffix,
        versions_mode,
        // The broadcast-bridge shim is test-only; it never surfaces metrics.
        Arc::new(crate::metrics::PipelineMetrics::disabled()),
        // Test shim never opts into chunking.
        false,
        // X1 — the legacy bridge wrapper uses the built-in default extension
        // map; production drives `run_with_ingest` directly with the resolved
        // per-kb map.
        crate::extmap::ExtensionMap::default(),
        // X2 — likewise an empty default gate (no exclusions, unpaused).
        crate::exclusions::IngestGate::default(),
        // Test-only bridge: private empty cache (no relocate peer).
        empty_dedup_cache(),
    )
    .await;
    bridge.abort();
}

/// Build the indexer for one kb. Consumes [`WatchWork`] from the per-kb
/// back-pressured ingest channel; producers (live watcher, reconciler walk +
/// delete pass, operator reindex, route nudges) push through an [`IngestSink`]
/// which also mirrors a `watch.*` frame to the observability bus. Runs until
/// every sender drops (channel closed = shutdown). The indexer reads ONLY this
/// channel, never the shared bus, so it can't lag on unrelated bus traffic.
#[allow(clippy::too_many_arguments)]
pub async fn run_with_ingest(
    kb_name: KbName,
    source_slug: SourceSlug,
    storage: StorageHandle,
    bus: Arc<EventBus>,
    quarantine_dir: PathBuf,
    mut rx: mpsc::Receiver<WatchWork>,
    embedder: EmbedderHandle,
    review_dir: Option<PathBuf>,
    // R2 — the kb's shared per-kb comment lock, held while the delete cascade
    // removes the `.review` sidecar + attachments (invariants #6 / #18). `None`
    // in tests / the broadcast-bridge shim, where nothing races a comment route.
    review_lock: Option<Arc<tokio::sync::Mutex<()>>>,
    artifact_host_suffix: String,
    versions_mode: crate::vcs::VersionsMode,
    metrics: Arc<crate::metrics::PipelineMetrics>,
    chunked: bool,
    // X1 — the kb's resolved extension map, used at the parse-dispatch seam in
    // `index_file` to pick Html vs Markdown. Per-kb-immutable for the task's
    // life (like `artifact_host_suffix` / `versions_mode` / `chunked`). The
    // sink-carried copy (used by the walk/watcher/reconcile gates) and this one
    // are the SAME resolved map — `bring_up_kb` builds it once.
    extensions: crate::extmap::ExtensionMap,
    // X2 — the kb's SHARED ingest gate (same `Arc` the sinks carry). Consulted
    // per item at the top of `prepare_doc` — the backstop seam that catches
    // whatever the producer-side gates (walk/watcher) let slip: an in-flight
    // event that raced an exclusion, or an operator reindex on a paused
    // source.
    gate: crate::exclusions::IngestGate,
    // FU1 — shared with relocate (`RelocateCtx.dedup`). Caller creates the
    // Arc; this task pre-populates it from lance below.
    indexed_hashes: DedupCache,
) {
    // Resolve the source root once. The indexer derives each artifact's
    // id by hashing its path relative to this root, so the id stays
    // stable across content edits. `upsert_source` always runs before
    // the indexer is spawned (daemon startup + test `setup()` alike);
    // the empty-path fallback degrades `from_path` to hashing the
    // absolute path — still unique per file.
    //
    // Match on `raw_slug` (the DB string form): `SourceRow.slug` is a
    // hardcoded placeholder today — see `storage::sqlite::list_sources`.
    let source_root: PathBuf = storage
        .list_sources()
        .await
        .ok()
        .and_then(|rows| {
            rows.into_iter()
                .find(|s| s.raw_slug == source_slug.as_str())
        })
        .map(|s| s.path)
        .unwrap_or_default();
    // v0.5 P4 — in-process anchor-stale tracker. Records
    // `(artifact_id, comment_id)` keys for comments whose last
    // fuzzy_resolve_anchor returned Stale; clears the key on a
    // Stale → non-Stale transition and emits `comment.anchor_resolved`
    // then.
    //
    // v0.7.1 P2 — keyed on `(artifact_id, comment_id)`. v0.7 keyed on
    // `(kb, comment_id)` on the (now-false) assumption that artifact_id
    // changed with content; since 8f26af0 the id is path-stable, and
    // the SPA's comment ids are NOT guaranteed unique across artifacts
    // — so the bare-comment-id key let one artifact's resolve clear a
    // *different* artifact's stale flag. The indexer is per-kb, so the
    // kb is implicit (it was always redundant in the key).
    //
    // v0.7 P1 — the set is persisted to a per-kb sidecar
    // (`<state>/<kb>/.anchors-stale.json`), so a comment that went
    // stale in a previous daemon process can still resolve (and emit
    // `comment.anchor_resolved`) on the next reindex.
    // #4 — value carries the v3 metadata (anchor_kind + fuzzy_score)
    // so cold-loaded entries round-trip back to the sidecar without
    // losing what the live event captured.
    let anchor_state: Arc<Mutex<HashMap<(String, String), crate::anchors::StaleAnchorEntry>>> =
        Arc::new(Mutex::new(HashMap::new()));
    if let Some(dir) = review_dir.as_deref() {
        let sidecar = crate::anchors::sidecar_path(dir);
        let mut guard = anchor_state.lock().unwrap_or_else(|e| e.into_inner());
        for (key, meta) in crate::anchors::load(&sidecar) {
            guard.insert(key, meta);
        }
    }

    // In-memory content-hash cache (artifact_id → [`IndexedMeta`]).
    // Skips the parse/embed/upsert/emit pipeline when a `watch.create`/
    // `watch.modify` arrives for a file whose bytes match the last
    // successfully-indexed version. Bytes-identical re-indexes used to
    // fire spurious `artifact.indexed` events whenever an editor's save
    // touched mtime without changing content, or a metadata-only fs
    // event slipped past the debouncer; the SPA detail view
    // (`web/src/routes/detail.tsx`) remounts its cross-origin iframe on
    // each such event, losing the user's scroll position.
    //
    // FU1 — the Arc is owned outside this task (created at daemon bring-up,
    // shared with relocate via the per-kb route context) so a move can
    // rekey old_id→new_id and the post-rename Created event hits this gate
    // without a redundant re-embed. This task still owns pre-population
    // and all runtime insert/heal/delete mutations.
    //
    // v0.16 — pre-populated at startup from the persisted `content_hash`
    // lance column. Before this, the cache started empty on every
    // restart, so the watcher's initial-walk `watch.create` events
    // re-embedded the entire corpus on every daemon start (kb-embedder
    // worker pegged at 100% CPU until the walk drained). Rows indexed
    // before v0.16 have NULL `content_hash` and aren't in the snapshot;
    // they take one more embed pass before being deduped on subsequent
    // restarts.
    //
    // v0.24 SC1 — each entry also carries the STORED `mtime_unix`, so the
    // dedup pre-gate's heal can compare disk vs stored instead of firing a
    // `touch_mtime` write on every duplicate emission (the restart-with-
    // backlog storm; see `IndexedMeta`).
    match storage.list_content_hashes().await {
        Ok(rows) => {
            let mut guard = indexed_hashes.lock().unwrap_or_else(|e| e.into_inner());
            for (id, hash, mtime) in rows {
                guard.insert(
                    id,
                    IndexedMeta {
                        content_hash: hash,
                        mtime_unix: mtime,
                    },
                );
            }
            tracing::debug!(
                kb = %kb_name,
                cached = guard.len(),
                "indexer: pre-populated content-hash cache from lance",
            );
        }
        Err(e) => {
            tracing::warn!(
                kb = %kb_name,
                error = %e,
                "indexer: list_content_hashes failed; startup re-embed unavoidable this run",
            );
        }
    }

    // G7 — drain the per-kb ingest channel. `recv()` returns `None` only when
    // EVERY sender (watcher sink, reconciler, route sinks) has dropped, i.e.
    // the kb is being torn down. No `Lagged` (the channel back-pressures), no
    // kb-filter (the channel is per-kb), no Lagged→re-walk escalation (nothing
    // to recover — the bounded channel blocks producers instead of dropping).
    while let Some(work) = rx.recv().await {
        tracing::debug!(
            kb = %kb_name,
            kind = ?work.kind,
            path = %work.path.display(),
            "indexer recv",
        );
        match work.kind {
            // `force = true` bypasses the byte-identical dedup gate — set by
            // the operator reindex walk. Live watcher events + reconciler
            // walks omit it.
            WatchKind::Created | WatchKind::Modified => {
                // GC-B7 — opportunistically grow `work` into a batch of
                // same-kind pending items so their storage upserts share one
                // Lance commit. `drain_ingest_batch` never blocks (try_recv
                // only), so a live-watcher trickle still indexes one file at
                // a time with no added latency; only a genuine backlog
                // benefits.
                let (batch, trailing_delete) = drain_ingest_batch(work, &mut rx);
                process_ingest_batch(
                    batch,
                    &kb_name,
                    &source_slug,
                    &source_root,
                    &storage,
                    &bus,
                    &quarantine_dir,
                    embedder.as_ref(),
                    review_dir.as_deref(),
                    anchor_state.clone(),
                    indexed_hashes.clone(),
                    &artifact_host_suffix,
                    versions_mode,
                    &metrics,
                    chunked,
                    &extensions,
                    &gate,
                )
                .await;
                // A `Deleted` item that ended the drain runs AFTER the batch
                // it was drained alongside, preserving the original recv
                // FIFO order (a create-then-delete-same-path pair must still
                // delete last).
                if let Some(del) = trailing_delete {
                    process_delete(
                        &kb_name,
                        &source_slug,
                        &source_root,
                        &storage,
                        &bus,
                        &del.path,
                        indexed_hashes.clone(),
                        review_dir.as_deref(),
                        review_lock.as_deref(),
                        del.keep_user_data,
                    )
                    .await;
                }
            }
            WatchKind::Deleted => {
                process_delete(
                    &kb_name,
                    &source_slug,
                    &source_root,
                    &storage,
                    &bus,
                    &work.path,
                    indexed_hashes.clone(),
                    review_dir.as_deref(),
                    review_lock.as_deref(),
                    work.keep_user_data,
                )
                .await;
            }
        }
    }
}

/// The keys the content-hash dedup gate compares: the path-based
/// `ArtifactId` (stable across content edits) and the content hash of the
/// file's bytes, plus the source-relative path the row is stored under.
/// Shared by `process_one`'s cheap pre-gate and `index_file`'s in-band
/// gate so the two can't silently diverge. Returns
/// `(artifact_id, content_hash, rel_path)`.
///
/// The empty-rel fallback (path not under `source_root`) shouldn't happen
/// for watcher-emitted paths, but if it did, hashing the absolute path
/// still yields a unique id.
/// Shared in-memory content-hash dedup cache (artifact_id → [`IndexedMeta`]).
///
/// Created once per kb at daemon bring-up, cloned onto the per-kb route
/// context and into the indexer task. Pre-populated from lance at indexer
/// startup (`list_content_hashes`), inserted at the success tail
/// (`finish_indexed_doc`), updated by the pre-gate's mtime heal, rekeyed by
/// the relocate engine on move (so the post-rename `Created` event hits the
/// gate — no re-embed), and invalidated on delete (`process_delete`).
///
/// Hold the `Mutex` only in short synchronous scopes — never across `.await`
/// (invariant #15).
pub type DedupCache = Arc<Mutex<HashMap<String, IndexedMeta>>>;

/// Empty shared cache (bring-up before lance pre-population; test fixtures).
pub fn empty_dedup_cache() -> DedupCache {
    Arc::new(Mutex::new(HashMap::new()))
}

fn identity_for(path: &Path, source_root: &Path, bytes: &[u8]) -> (ArtifactId, String, String) {
    let rel_path = crate::paths::doc_rel_path(&path.to_string_lossy(), source_root);
    let artifact_id = if rel_path.is_empty() {
        ArtifactId::from_path(&path.to_string_lossy())
    } else {
        ArtifactId::from_path(&rel_path)
    };
    let content_hash = ArtifactId::from_html_bytes(bytes).as_str().to_string();
    (artifact_id, content_hash, rel_path)
}

/// GC-B7 — opportunistically grow `first` into a same-kind (Created/
/// Modified) run of pending work so their storage upserts share one
/// `upsert_docs` batch instead of `INGEST_BATCH_MAX_DOCS` separate Lance
/// commits. `try_recv` never blocks — an empty channel just ends the batch
/// at whatever's already queued, so a slow live-watcher trickle (the common
/// case) still indexes one file at a time with no added latency; only a
/// genuine backlog (bulk cold-import, catch-up reconcile) benefits. Stops
/// at a `Deleted` item WITHOUT reordering it ahead of the batch — FIFO recv
/// order stays intact (a create-then-delete-same-path pair must still
/// delete last); the caller runs it right after the batch it's returned
/// alongside.
fn drain_ingest_batch(
    first: WatchWork,
    rx: &mut mpsc::Receiver<WatchWork>,
) -> (Vec<WatchWork>, Option<WatchWork>) {
    let mut batch = vec![first];
    while batch.len() < INGEST_BATCH_MAX_DOCS {
        match rx.try_recv() {
            Ok(next) if matches!(next.kind, WatchKind::Created | WatchKind::Modified) => {
                batch.push(next);
            }
            Ok(next) => return (batch, Some(next)),
            Err(_) => break,
        }
    }
    (batch, None)
}

/// GC-B7 — one file's worth of parsed/embedded state, ready for a batched
/// `storage.upsert_docs` call. Produced by `prepare_doc` (read → parse →
/// embed — everything the drain-batching loop can safely do BEFORE the
/// storage commit); consumed by `finish_indexed_doc` (chunks → enrichment →
/// events → dedup-cache — everything that assumes the doc's row already
/// exists). Splitting the old `index_file` pipeline here is what lets N
/// files share ONE Lance commit instead of N.
struct PreparedDoc {
    path: PathBuf,
    run_id: RunId,
    started: Instant,
    change_kind: ChangeKind,
    artifact_id: ArtifactId,
    content_hash: String,
    rel_path: String,
    html: String,
    raw: String,
    session_parse: Option<crate::sessions::SessionParse>,
    doc: Doc,
    chunk_docs: Vec<crate::storage::schema::ChunkDoc>,
    enrich_category: Option<String>,
    enrich_mtime_unix: i64,
    seed_global: bool,
    seed_linked_kbs: Vec<String>,
    /// 2026-08-21 ci-host incident hotfix: set when the quarantine gate skipped
    /// this doc's embed (see `prepare_doc`'s `embed_gated`). `finish_
    /// indexed_doc`'s "successful index clears every open error for this
    /// path" step (below) must NOT run for a gated doc — the whole point of
    /// the gate is that the retry_count/quarantine state PERSISTS across
    /// reconcile passes for an unchanged content_hash, so a gated pass must
    /// leave the error row alone (a normal successful index is proof the
    /// failure condition is gone; a gated one is proof of nothing — the
    /// embed was never attempted).
    embed_gated: bool,
}

/// Per-file run-completion bookkeeping shared by every exit path (the
/// in-band dedup no-op, a prepare-stage failure, and a successfully
/// upserted doc) — verbatim the tail the pre-GC-B7 `process_one` ran inline
/// before the pipeline was split so multiple docs could share one storage
/// commit.
#[allow(clippy::too_many_arguments)]
async fn finish_run_tail(
    bus: &EventBus,
    storage: &StorageHandle,
    kb_name: &KbName,
    metrics: &crate::metrics::PipelineMetrics,
    run_id: &RunId,
    path: &Path,
    started: Instant,
    ok: bool,
) {
    let elapsed_ms = started.elapsed().as_millis() as u64;
    // TM-track — per-file index wall time (read→parse→embed→upsert→enrich).
    // Only reached for files that actually ran a full pass; the dedup
    // pre-gate bails before this, so unchanged files aren't counted.
    metrics.observe_index_file(elapsed_ms);
    let (ok_count, err_count) = if ok { (1, 0) } else { (0, 1) };
    bus.emit(
        "index.file",
        json!({
            "run": run_id.as_str(),
            "kb": kb_name.as_str(),
            "path": path.to_string_lossy(),
            "ms": elapsed_ms,
            "edges": 0,
            "ok": ok,
        }),
    );
    let _ = storage
        .finish_run(run_id.clone(), ok_count, err_count, unix_now())
        .await;
    bus.emit(
        "index.complete",
        json!({
            "run": run_id.as_str(),
            "kb": kb_name.as_str(),
            "ok_count": ok_count,
            "err_count": err_count,
            "duration_ms": elapsed_ms,
        }),
    );
}

/// GC-B7 — drive a drained batch of same-kind `WatchWork` through the
/// prepare/upsert-batch/finish pipeline. Per-file bookkeeping (the dedup
/// pre-gate, `begin_run`/`index.start`, and the `finish_run_tail` every exit
/// path ends in) is unchanged from the pre-batching `process_one` — only
/// the storage commit itself is shared across the batch (`flush_prepared_batch`).
#[allow(clippy::too_many_arguments)]
async fn process_ingest_batch(
    items: Vec<WatchWork>,
    kb_name: &KbName,
    source_slug: &SourceSlug,
    source_root: &Path,
    storage: &StorageHandle,
    bus: &EventBus,
    quarantine_dir: &Path,
    embedder: Option<&Arc<Mutex<Embedder>>>,
    review_dir: Option<&Path>,
    anchor_state: Arc<Mutex<HashMap<(String, String), crate::anchors::StaleAnchorEntry>>>,
    indexed_hashes: DedupCache,
    artifact_host_suffix: &str,
    versions_mode: crate::vcs::VersionsMode,
    metrics: &crate::metrics::PipelineMetrics,
    chunked: bool,
    // X1 — forwarded verbatim to `prepare_doc` for the parse-pipeline dispatch.
    extensions: &crate::extmap::ExtensionMap,
    // X2 — forwarded verbatim to `prepare_doc` for the per-item gate check.
    gate: &crate::exclusions::IngestGate,
) {
    let mut prepared: Vec<PreparedDoc> = Vec::with_capacity(items.len());
    let mut pending_bytes: usize = 0;

    for work in items {
        let change_kind = if work.kind == WatchKind::Created {
            ChangeKind::Created
        } else {
            ChangeKind::Modified
        };

        // Dedup PRE-gate — unchanged from the pre-GC-B7 `process_one`: the
        // in-band gate inside `prepare_doc` (below) skips the embed/upsert
        // for byte-identical content, but it runs AFTER `begin_run` +
        // `index.start` — so on the safety-net reconcile pass (which
        // re-emits a `watch.modify` for EVERY file every `reconcile_secs`)
        // each unchanged file would still spin up a sqlite run row and a
        // full `index.start`/`index.file`/`index.complete` triple. Reading +
        // hashing here and bailing on a confirmed hit BEFORE `begin_run`
        // makes a no-op pass truly silent.
        if !work.force {
            if let Ok(bytes) = tokio::fs::read(&work.path).await {
                let (artifact_id, content_hash, _rel_path) =
                    identity_for(&work.path, source_root, &bytes);
                let (unchanged, stored_mtime) = {
                    let guard = indexed_hashes.lock().unwrap_or_else(|e| e.into_inner());
                    match guard.get(artifact_id.as_str()) {
                        Some(cached) if cached.content_hash == content_hash => {
                            (true, cached.mtime_unix)
                        }
                        _ => (false, None),
                    }
                };
                if unchanged {
                    // Heal a stale stored mtime before bailing — see the
                    // original rationale (a `git checkout`-touched-but-
                    // unchanged file re-emits `watch.modify` forever
                    // otherwise). v0.24 SC1: heal ONLY when the on-disk
                    // mtime actually differs from the STORED one. This used
                    // to fire unconditionally on every dedup-skip, turning
                    // each duplicate emission of an unchanged file (restart
                    // initial-walk, reconcile ticks over a live backlog,
                    // overflow rescans) into a full-table-scan lance UPDATE
                    // + one manifest commit, serialized on the single-writer
                    // actor ahead of real ingest — the restart-with-backlog
                    // quadratic storm (20k corpus: drain collapsed to
                    // ~1 doc/s, manifests 269→1,994, 2 GB RSS in the scale
                    // lab). Duplicates carry an unchanged mtime, so this
                    // compare makes them write-free; a genuine touch still
                    // heals exactly once (self-limiting, as before) and
                    // teaches the cache the new value.
                    if let Some(disk_mtime) = tokio::fs::metadata(&work.path)
                        .await
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64)
                    {
                        if stored_mtime != Some(disk_mtime) {
                            match storage
                                .touch_mtime(artifact_id.as_str().to_string(), disk_mtime)
                                .await
                            {
                                Ok(()) => {
                                    tracing::debug!(
                                        kb = %kb_name,
                                        artifact = %artifact_id,
                                        stored = ?stored_mtime,
                                        disk = disk_mtime,
                                        "dedup pre-gate healed stale stored mtime",
                                    );
                                    let mut guard =
                                        indexed_hashes.lock().unwrap_or_else(|e| e.into_inner());
                                    if let Some(meta) = guard.get_mut(artifact_id.as_str()) {
                                        meta.mtime_unix = Some(disk_mtime);
                                    }
                                }
                                Err(e) => {
                                    tracing::debug!(kb = %kb_name, error = %e, "mtime heal failed (non-fatal)");
                                }
                            }
                        }
                    }
                    continue;
                }
            }
        }

        let started = Instant::now();
        let now_unix = unix_now();
        let run_id = match storage.begin_run(source_slug.clone(), now_unix).await {
            Ok(id) => id,
            Err(e) => {
                tracing::error!(kb = %kb_name, error = %e, "begin_run failed");
                continue;
            }
        };
        bus.emit(
            "index.start",
            json!({
                "run": run_id.as_str(),
                "kb": kb_name.as_str(),
                "src": source_slug.as_str(),
                "total": 1,
            }),
        );

        match prepare_doc(
            kb_name,
            source_slug,
            source_root,
            storage,
            bus,
            quarantine_dir,
            &work.path,
            change_kind,
            work.force,
            embedder,
            &run_id,
            indexed_hashes.clone(),
            metrics,
            chunked,
            extensions,
            gate,
        )
        .await
        {
            Ok(Some(p)) => {
                // Byte-budget the batch: a doc whose body+html+raw would
                // push the running total over INGEST_BATCH_MAX_BYTES
                // flushes what's already queued FIRST (see the const's
                // rationale).
                let cost = p.doc.body.len() + p.html.len() + p.raw.len();
                if !prepared.is_empty()
                    && pending_bytes.saturating_add(cost) > INGEST_BATCH_MAX_BYTES
                {
                    flush_prepared_batch(
                        std::mem::take(&mut prepared),
                        kb_name,
                        source_slug,
                        source_root,
                        storage,
                        bus,
                        quarantine_dir,
                        review_dir,
                        anchor_state.clone(),
                        indexed_hashes.clone(),
                        artifact_host_suffix,
                        versions_mode,
                        metrics,
                        chunked,
                    )
                    .await;
                    pending_bytes = 0;
                }
                pending_bytes += cost;
                prepared.push(p);
            }
            Ok(None) => {
                // In-band dedup gate hit (rare — the pre-gate above already
                // covers the common case): nothing to upsert, but the file
                // WAS successfully considered, so the run finishes `ok`.
                finish_run_tail(
                    bus, storage, kb_name, metrics, &run_id, &work.path, started, true,
                )
                .await;
            }
            Err(()) => {
                // `prepare_doc` already called `record_failure` internally.
                finish_run_tail(
                    bus, storage, kb_name, metrics, &run_id, &work.path, started, false,
                )
                .await;
            }
        }
    }

    if !prepared.is_empty() {
        flush_prepared_batch(
            prepared,
            kb_name,
            source_slug,
            source_root,
            storage,
            bus,
            quarantine_dir,
            review_dir,
            anchor_state,
            indexed_hashes,
            artifact_host_suffix,
            versions_mode,
            metrics,
            chunked,
        )
        .await;
    }
}

/// GC-B7 — commit a whole prepared batch in ONE Lance `merge_insert`
/// (`storage::lance::Storage::upsert_docs`, previously wired but unused by
/// this path — see the 2026-07-11 scale-test report). `merge_insert` is a
/// single atomic commit, so a failure doesn't tell us WHICH doc was the
/// problem (e.g. a stray schema/dim mismatch on one embedding); the bisect
/// fallback below retries singularly so one bad doc can't sink the rest of
/// the batch. That's the rare path — a healthy corpus never hits it — so
/// the extra per-doc round trips there are an acceptable cost for full
/// error isolation.
#[allow(clippy::too_many_arguments)]
async fn flush_prepared_batch(
    batch: Vec<PreparedDoc>,
    kb_name: &KbName,
    source_slug: &SourceSlug,
    source_root: &Path,
    storage: &StorageHandle,
    bus: &EventBus,
    quarantine_dir: &Path,
    review_dir: Option<&Path>,
    anchor_state: Arc<Mutex<HashMap<(String, String), crate::anchors::StaleAnchorEntry>>>,
    indexed_hashes: DedupCache,
    artifact_host_suffix: &str,
    versions_mode: crate::vcs::VersionsMode,
    metrics: &crate::metrics::PipelineMetrics,
    chunked: bool,
) {
    let docs: Vec<Doc> = batch.iter().map(|p| p.doc.clone()).collect();
    if storage.upsert_docs(docs).await.is_ok() {
        for p in batch {
            finish_indexed_doc(
                p,
                kb_name,
                source_slug,
                source_root,
                storage,
                bus,
                quarantine_dir,
                review_dir,
                anchor_state.clone(),
                indexed_hashes.clone(),
                artifact_host_suffix,
                versions_mode,
                metrics,
                chunked,
            )
            .await;
        }
        return;
    }

    for p in batch {
        match storage.upsert_doc(p.doc.clone()).await {
            Ok(()) => {
                finish_indexed_doc(
                    p,
                    kb_name,
                    source_slug,
                    source_root,
                    storage,
                    bus,
                    quarantine_dir,
                    review_dir,
                    anchor_state.clone(),
                    indexed_hashes.clone(),
                    artifact_host_suffix,
                    versions_mode,
                    metrics,
                    chunked,
                )
                .await;
            }
            Err(e) => {
                let _ = record_failure(
                    kb_name,
                    source_slug,
                    storage,
                    bus,
                    quarantine_dir,
                    &p.path,
                    "storage",
                    format!("upsert: {e}"),
                    Some(p.content_hash.clone()),
                )
                .await;
                finish_run_tail(
                    bus, storage, kb_name, metrics, &p.run_id, &p.path, p.started, false,
                )
                .await;
            }
        }
    }
}

/// Query-priority lane (2026-07 tail-latency fix): before the indexer takes
/// the shared embedder for a doc vector or a chunk mini-batch, briefly yield
/// it to any query waiting to embed on the same model — those mark themselves
/// via [`crate::embed::QueryLaneGuard`]. A no-op (zero cost) when no query is
/// contending, which is the steady state. Bounded so a sustained query stream
/// delays — never indefinitely starves — background indexing.
async fn yield_to_pending_queries(model: &'static str) {
    // Query embeds are ~50–170 ms and drain fast, so a handful of waiters
    // clear well within the cap; the cap only bites under a relentless query
    // storm, where letting indexing inch forward is the right trade.
    const MAX_YIELD_MS: u64 = 500;
    let started = Instant::now();
    while crate::embed::pending_query_count(model) > 0 {
        if started.elapsed() >= std::time::Duration::from_millis(MAX_YIELD_MS) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
}

/// [`MAX_CHUNKS_PER_DOC`] enforcement, parameterised on `cap` — same house
/// pattern as `sessions::truncate_code_field_to`'s `budget` param — so a
/// unit test can exercise the truncation branch against a handful of
/// chunks instead of constructing 512+ real ones. Keeps the FIRST `cap`
/// chunks (reading order — chunk-0 survives whenever `cap >= 1`) and
/// returns the ORIGINAL count alongside the (possibly truncated) list, so
/// the caller can tell whether anything was capped without a second
/// `.len()` comparison against a moved-from vec.
fn cap_chunks_to(
    chunks: Vec<crate::chunk::Chunk>,
    cap: usize,
) -> (Vec<crate::chunk::Chunk>, usize) {
    let total = chunks.len();
    let mut chunks = chunks;
    if total > cap {
        chunks.truncate(cap);
    }
    (chunks, total)
}

/// GC-B7 — phase 1 of the (former `index_file`) pipeline: read → parse →
/// embed → build the `Doc` + chunk vectors, everything the drain-batching
/// loop can safely do BEFORE the storage commit. Returns `Ok(Some(_))` when
/// there's a doc ready to upsert, `Ok(None)` on the in-band dedup-gate
/// no-op, and `Err(())` on a genuine failure (already recorded via
/// `record_failure` internally — the caller only needs to know pass/fail
/// for its own `finish_run_tail`). `finish_indexed_doc` is phase 2: chunks →
/// enrichment → events → dedup-cache, everything that assumes the row
/// already exists in Lance.
#[allow(clippy::too_many_arguments)]
async fn prepare_doc(
    kb_name: &KbName,
    source_slug: &SourceSlug,
    source_root: &Path,
    storage: &StorageHandle,
    bus: &EventBus,
    quarantine_dir: &Path,
    path: &Path,
    change_kind: ChangeKind,
    force: bool,
    embedder: Option<&Arc<Mutex<Embedder>>>,
    run_id: &RunId,
    indexed_hashes: DedupCache,
    metrics: &crate::metrics::PipelineMetrics,
    chunked: bool,
    // X1 — the resolved extension map. Selects the parse pipeline below
    // (`map.pipeline(path)`), replacing the hardcoded `is_markdown` gate. D1:
    // it resolves to exactly Html or Markdown — no third arm, no new parser.
    extensions: &crate::extmap::ExtensionMap,
    // X2 — the shared ingest gate, checked FIRST (below).
    gate: &crate::exclusions::IngestGate,
) -> Result<Option<PreparedDoc>, ()> {
    // X2/D6 — the per-item enforcement backstop, at the very TOP (before the
    // read): an excluded file, or ANY file of a paused source, exits through
    // the existing `Ok(None)` skip arm — the run finishes ok, nothing is
    // parsed/embedded/upserted. This is what makes a producer-side race
    // harmless: an event already in the ingest channel when the exclusion /
    // pause landed still can't re-enter the index.
    if gate.paused() || gate.is_excluded(path, source_root) {
        return Ok(None);
    }

    // Read. M5: `tokio::fs::read` so a slow filesystem (NFS, FUSE) doesn't
    // block the runtime thread that's also serving HTTP / SSE. The
    // reconciler can fan out hundreds of these per pass.
    let bytes = match tokio::fs::read(path).await {
        Ok(b) => b,
        Err(e) => {
            let _ = record_failure(
                kb_name,
                source_slug,
                storage,
                bus,
                quarantine_dir,
                path,
                "io",
                format!("read failed: {e}"),
                None,
            )
            .await;
            return Err(());
        }
    };

    // Identity is the source-relative path (stable across content
    // edits); the content hash is kept separately, only for change /
    // retry detection.
    let (artifact_id, content_hash, rel_path) = identity_for(path, source_root, &bytes);

    // Content-hash dedup gate (in-band). `process_ingest_batch`'s PRE-gate
    // already short-circuits the common unchanged-file case before this
    // function is even called (so reconcile no-ops emit nothing); this
    // in-band copy remains the backstop for the paths that reach here
    // anyway — a genuinely-changed file whose bytes happened to revert between the
    // pre-gate read and here, and any future caller that skips the
    // pre-gate. A `watch.modify` for byte-identical
    // content (mtime touch, editor save with no diff, perm change that
    // crossed `ModifyKind::Metadata`) used to fire a full reindex +
    // `artifact.indexed`. The SPA detail view remounts its cross-origin
    // iframe on every such event, losing scroll. Skip parse/embed/
    // upsert/edges/anchor/emit when the new hash matches the cache.
    //
    // v0.16 — gates BOTH `Created` and `Modified`. The cache is
    // pre-populated from lance at indexer startup (see
    // `storage.list_content_hashes` in `run`), so the watcher's
    // initial-walk `watch.create` envelopes for unchanged artifacts no
    // longer trigger a full re-embed on every daemon restart.
    // Rename-as-create is unaffected: artifact_id is path-based, so a
    // renamed file gets a fresh id and the cache misses naturally.
    //
    // `force` bypasses the gate — set by explicit operator reindex
    // routes (kb_post / post). Necessary because cross-artifact link
    // resolution can fail on initial walk (target not yet in storage)
    // and the operator's reindex is the recovery path. The reconciler's
    // walk uses force=false so byte-identical files still short-circuit
    // cheaply on the safety-net pass.
    //
    // Safety for !force: anchor-stale/resolved transitions are a
    // function of `(html bytes, anchor)`, so identical bytes → no
    // transition. Outbound edges are byte-derived too; identical bytes
    // → identical set (once both endpoints have been indexed at least
    // once). The dedup gate also skips the auto-dismiss-on-success path
    // — but a same-hash dedup-skip means the prior index for this
    // (path, hash) already cleared the error rows, so re-running it
    // wouldn't dismiss anything new. See plan
    // `/home/user/.claude/plans/while-i-m-reading-an-jaunty-shannon.md`
    // for the full rationale.
    if !force {
        let unchanged = indexed_hashes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(artifact_id.as_str())
            .is_some_and(|cached| cached.content_hash == content_hash);
        if unchanged {
            return Ok(None);
        }
    }

    // Stat for size_kb / mtime / btime. M5: async — same NFS rationale
    // as the read above.
    let metadata = tokio::fs::metadata(path).await.ok();
    let size_kb = metadata
        .as_ref()
        .map(|m| (m.len() / 1024) as u32)
        .unwrap_or(0);
    let mtime_unix = metadata
        .as_ref()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // v0.15 — filesystem birth time. `Metadata::created()` returns Err
    // on filesystems that don't track btime (some network mounts); we
    // store None there so the column round-trips truthfully.
    let created_unix = metadata
        .as_ref()
        .and_then(|m| m.created().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64);

    // Parse. Markdown sources render to an HTML fragment ONCE here (the same
    // `render_fragment` the serve path wraps), so `extract` and the link-edge
    // pass below both see the rendered HTML; the flat frontmatter overrides
    // the metadata facets. HTML sources pass through untouched.
    let raw = match std::str::from_utf8(&bytes) {
        Ok(s) => s,
        Err(e) => {
            let _ = record_failure(
                kb_name,
                source_slug,
                storage,
                bus,
                quarantine_dir,
                path,
                "parse",
                format!("not valid utf-8: {e}"),
                Some(content_hash),
            )
            .await;
            return Err(());
        }
    };

    // X1 — pick the parse pipeline from the resolved extension map, NOT the
    // hardcoded `is_markdown` gate: a config-mapped extension (e.g. `.txt` →
    // Markdown) must render through `extract_markdown`. D1: the map resolves to
    // exactly Html or Markdown, so this stays a two-arm dispatch with no new
    // parser. An unmapped extension (defensive — the ingest gate should have
    // filtered it) falls through to the HTML pipeline, matching the pre-X1
    // `else` branch.
    let use_markdown = matches!(
        extensions.pipeline(path),
        Some(crate::extmap::Pipeline::Markdown)
    );
    let md_html_owned: String;
    let (mut fields, html): (parser::Fields, &str) = if use_markdown {
        let (f, rendered) = parser::extract_markdown(raw);
        md_html_owned = rendered;
        (f, md_html_owned.as_str())
    } else {
        (parser::extract(raw), raw)
    };

    // R1 — a session transcript's parsed <body> is raw, multi-MB JSONL: it
    // retrieves badly for itself and would bury `kb recollect`. Replace the
    // index's view of the body + excerpt with a deterministic INSIGHT DIGEST
    // built from the already-parsed session activity. EVERY indexed view
    // derives from these two fields — the doc-vector embed (`embed_body`
    // below), the BM25 `body` column (`doc.body`), the SQ5 chunk source, and
    // the `body_text_excerpt` — so this single substitution covers them all.
    // The raw transcript on disk is untouched (invariant #27); only what the
    // index matches against changes. (R4 enriches the digest with extracted
    // research queries via the same shared parse.)
    // The parse is kept alive past the digest and lent to the enrichment
    // ctx below, so the session-capture hook reuses it instead of JSON-
    // parsing the same multi-MB transcript a second time per capture.
    // (`parse_session_html_full`'s filename-derived fields strip any
    // directory prefix themselves, so passing `file_name()` here and
    // `rel_path` in the hook's fallback yields the same result.)
    let mut session_parse: Option<crate::sessions::SessionParse> = None;
    if fields.kb_category.as_deref() == Some(crate::sessions::MEMORY_SESSION_CATEGORY) {
        let filename = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        let parse = crate::sessions::parse_session_html_full(html, filename, mtime_unix);
        let digest = crate::sessions::session_digest(&parse);
        if !digest.trim().is_empty() {
            fields.body_text_excerpt = crate::sessions::session_digest_excerpt(&parse);
            fields.body = digest;
            // `fields.code` is LEFT AS THE PARSER SET IT, then CAPPED —
            // it is no longer overwritten to empty (operator decision,
            // 2026-07-21): `code` is the FULL-TEXT BM25 evidence lane for a
            // session — every exact token that ever appeared anywhere in it
            // should be findable — while `body`/embed/SQ5-chunks/excerpt
            // (above) stay the R1 digest, the small high-signal
            // ranking-rationale surface. The parser's `code_blocks` selector
            // (`pre code, pre`, parser.rs) is a document-wide CSS sweep, not
            // a `<body>`-scoped walk: a `hidden` attribute and a
            // non-`<script>`/`<style>`/`<template>` ancestor are both
            // invisible to it. So `fields.code` ALREADY holds both the main
            // transcript `<pre>` AND every sidecar-text tail block's
            // `<details><pre>` (`sessions::SIDECAR_TEXT_BLOCK_ID`, present
            // only when the capture walked `agent-*.jsonl` sidecars) —
            // empirically pinned by
            // `parser::tests::code_field_includes_both_main_transcript_and_sidecar_text_pre_blocks`.
            //
            // W0.6 amendment (2026-07-22): the first ship left the MAIN
            // transcript portion completely uncapped (only the sidecar-text
            // block's own `SIDECAR_TEXT_AGENT_CAP_BYTES`/
            // `SIDECAR_TEXT_TOTAL_CAP_BYTES` budgets applied) — on the live
            // 916-row sessions corpus that let the corpus-wide sum of `code`
            // bytes exceed Arrow's `i32::MAX` (~2.147 GB) ceiling, panicking
            // `lance`'s `interleave_bytes` (`arrow-select`'s
            // `interleave.rs`) on `kb reindex`/every `recollect` call. `code`
            // is now capped WHOLE (main transcript + sidecar-text combined,
            // one guard, `sessions::truncate_code_field`) to
            // `sessions::SESSION_CODE_FIELD_CAP_BYTES` (32 KiB) — see that
            // constant's doc comment for the corpus-wide sizing arithmetic.
            // `code` carries no embedding weight (`embed_body` below reads
            // only `fields.body`), so this only affects BM25 exact-token
            // reach on `code` (`code` is one of `FTS_COLUMNS`), never vector
            // relevance.
            fields.code = crate::sessions::truncate_code_field(&fields.code);
        }
        session_parse = Some(parse);
    }

    // Quarantine gate (2026-08-21 ci-host incident): before this fix, a doc
    // already quarantined `QUARANTINE_THRESHOLD` times at this exact
    // content_hash still got a fresh doc-level + chunk embed attempt on
    // every hourly reconcile pass forever — the defect that let a 292MB
    // session HTML OOM-loop the embedder hourly (81 quarantine events,
    // RestartCount 198) even though it was quarantined after the first 3
    // failures. Query the SAME (path, content_hash) pair `record_failure`
    // uses so this reads the count it just wrote. `>=` (not `>`) because
    // `record_failure` already quarantines on the failure that makes
    // `retries + 1 >= QUARANTINE_THRESHOLD` true, so by the time the
    // stored count reaches the threshold the file is already quarantined.
    // Skips ONLY the embed passes below — the doc still indexes and is
    // BM25/keyword-searchable (the lance embedding column is nullable by
    // design, kb-core invariant #1). A content edit produces a different
    // content_hash, so the count resets to 0 and the doc gets a fresh
    // embed attempt — no extra state needed. `embed_gated` also travels on
    // `PreparedDoc` to `finish_indexed_doc`, which must skip its normal
    // "successful index clears every open error for this path" step for a
    // gated doc — otherwise that clear would wipe the very row this gate
    // reads, un-gating the doc on the next reconcile pass (see that
    // function's guard for the full rationale).
    let embed_gated = if embedder.is_some() {
        let retries = storage
            .retry_count_for_path_hash(path.to_path_buf(), content_hash.clone())
            .await
            .unwrap_or(0);
        let gated = retries >= QUARANTINE_THRESHOLD;
        if gated {
            bus.emit(
                "index.embed_skipped",
                json!({
                    "run": run_id.as_str(),
                    "kb": kb_name.as_str(),
                    "path": path.to_string_lossy(),
                    "retries": retries,
                }),
            );
            tracing::warn!(
                kb = %kb_name,
                path = %path.to_string_lossy(),
                retries,
                "skipping doc + chunk embed: already quarantined at this content_hash",
            );
        }
        gated
    } else {
        false
    };

    // Embed (slow path: ~170 ms per doc on Kaby Lake per spike-fastembed).
    // P4 — the embed is a blocking IPC round-trip to the kb-embedder
    // subprocess; running it on `spawn_blocking` keeps this tokio worker
    // responsive instead of parking it for ~170 ms. We emit the
    // `index.embedding` event up front under a brief lock just to read the
    // (stable, per-kb) model_name, then do the embed on a blocking thread —
    // splitting the two can't mismatch model_name vs the result because the
    // model never changes for a given embedder.
    let embedding = if embed_gated {
        None
    } else if let Some(emb) = embedder {
        let model = {
            let guard = emb.lock().unwrap_or_else(|e| e.into_inner());
            let model = guard.model_name();
            bus.emit(
                "index.embedding",
                json!({
                    "run": run_id.as_str(),
                    "kb": kb_name.as_str(),
                    "path": path.to_string_lossy(),
                    "model": model,
                    // N9: body byte count for the about-to-embed text.
                    // The TUI sums these into a fleet bytes-embedded
                    // counter on TRAFFIC; small payload addition with
                    // no breaking schema change (consumers reading
                    // index.embedding ignore unknown fields).
                    "bytes": fields.body.len() as u64,
                }),
            );
            model
        };
        // Query-priority lane: yield the shared embedder to any query waiting
        // on this model before taking it for the doc vector (2026-07 tail-
        // latency fix; see crate::embed::QueryLaneGuard).
        yield_to_pending_queries(model).await;
        let emb = Arc::clone(emb);
        let embed_body = fields.body.clone();
        // TM-track — index-side embed latency (distinct from the query-side
        // `search.embed_ms` in kb-server). Recorded regardless of outcome; a
        // failed embed still consumed time.
        let embed_started = Instant::now();
        let result = tokio::task::spawn_blocking(move || {
            let mut guard = emb.lock().unwrap_or_else(|e| e.into_inner());
            guard.embed_one(&embed_body)
        })
        .await;
        metrics.observe_embed_index(embed_started.elapsed().as_millis() as u64, 1);
        match result {
            Ok(Ok(v)) => Some(v),
            Ok(Err(e)) => {
                let _ = record_failure(
                    kb_name,
                    source_slug,
                    storage,
                    bus,
                    quarantine_dir,
                    path,
                    "embed",
                    format!("embed failed: {e}"),
                    Some(content_hash),
                )
                .await;
                return Err(());
            }
            Err(join_err) => {
                let _ = record_failure(
                    kb_name,
                    source_slug,
                    storage,
                    bus,
                    quarantine_dir,
                    path,
                    "embed",
                    format!("embed task panicked: {join_err}"),
                    Some(content_hash),
                )
                .await;
                return Err(());
            }
        }
    } else {
        None
    };

    // SQ5 — passage chunks (opt-in via `chunked`). Embed each chunk so
    // semantic search sees the whole document, not just the first ~400
    // words. Best-effort: a chunk-embed failure logs + skips, and search
    // falls back to the doc-level vector arm. Built here while the
    // embedder is hot; the doc-level embedding above is kept regardless
    // (atlas reads it). Gated by the SAME `embed_gated` computed above the
    // doc-level embed — an already-quarantined doc skips this pass too.
    let chunk_docs: Vec<crate::storage::schema::ChunkDoc> = match (chunked, embedder) {
        (true, Some(emb)) if !embed_gated => {
            let title_for_chunk = fields
                .title
                .clone()
                .or_else(|| fields.h1.clone())
                .unwrap_or_default();
            let chunks = crate::chunk::chunk_document_default(
                &title_for_chunk,
                &fields.headings,
                &fields.body,
            );
            let (chunks, total_chunks) = cap_chunks_to(chunks, MAX_CHUNKS_PER_DOC);
            if total_chunks > MAX_CHUNKS_PER_DOC {
                bus.emit(
                    "index.chunks_capped",
                    json!({
                        "run": run_id.as_str(),
                        "kb": kb_name.as_str(),
                        "path": path.to_string_lossy(),
                        "total": total_chunks,
                        "kept": MAX_CHUNKS_PER_DOC,
                    }),
                );
                tracing::warn!(
                    kb = %kb_name,
                    path = %path.to_string_lossy(),
                    total = total_chunks,
                    kept = MAX_CHUNKS_PER_DOC,
                    "chunk list exceeds MAX_CHUNKS_PER_DOC; truncated to the first N chunks (chunk-0 kept)",
                );
            }
            if chunks.is_empty() {
                Vec::new()
            } else {
                let texts: Vec<String> = chunks.iter().map(|c| c.text.clone()).collect();
                let model = {
                    let guard = emb.lock().unwrap_or_else(|e| e.into_inner());
                    guard.model_name()
                };
                // Query-priority lane: embed the chunk vectors in small
                // mini-batches, yielding the shared embedder between them
                // whenever a latency-sensitive query is waiting on the same
                // model. A single embed_batch over ALL chunks would hold the
                // mutex for the whole document — the reindex-vs-query
                // contention behind the 2026-07-03 recall stalls. A query now
                // waits at most one mini-batch (crate::embed::QueryLaneGuard +
                // INDEX_EMBED_MINI_BATCH).
                let mut all_vecs: Vec<Vec<f32>> = Vec::with_capacity(texts.len());
                let mut embed_ok = true;
                for mini in texts.chunks(crate::embed::INDEX_EMBED_MINI_BATCH) {
                    yield_to_pending_queries(model).await;
                    let emb = Arc::clone(emb);
                    let mini_texts = mini.to_vec();
                    let embedded = tokio::task::spawn_blocking(move || {
                        let mut guard = emb.lock().unwrap_or_else(|e| e.into_inner());
                        guard.embed_batch(&mini_texts)
                    })
                    .await;
                    match embedded {
                        Ok(Ok(vecs)) if vecs.len() == mini.len() => all_vecs.extend(vecs),
                        _ => {
                            embed_ok = false;
                            break;
                        }
                    }
                }
                if embed_ok && all_vecs.len() == chunks.len() {
                    chunks
                        .iter()
                        .zip(all_vecs)
                        .map(|(c, v)| crate::storage::schema::ChunkDoc {
                            chunk_id: format!("{}#{}", artifact_id.as_str(), c.idx),
                            doc_id: artifact_id.as_str().to_string(),
                            chunk_idx: c.idx,
                            text: c.text.clone(),
                            embedding: Some(v),
                        })
                        .collect()
                } else {
                    tracing::warn!(
                        kb = %kb_name,
                        path = %path.to_string_lossy(),
                        "chunk embed failed; skipping chunks (doc-vector search unaffected)"
                    );
                    Vec::new()
                }
            }
        }
        _ => Vec::new(),
    };

    let doc = Doc {
        id: artifact_id.as_str().to_string(),
        // Store the CANONICAL absolute path so a symlinked source root
        // (WSL/bind mounts) doesn't desync from
        // reconcile's canonical-root delete pass or `get_by_source_path`.
        // IDs are unaffected (derived from the source-relative path).
        path: crate::paths::canonical_abs(path)
            .to_string_lossy()
            .to_string(),
        title: fields.title.or(fields.h1).unwrap_or_else(|| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("untitled")
                .to_string()
        }),
        body: fields.body.clone(),
        headings: fields.headings,
        code: fields.code,
        prompt: fields.prompt,
        body_text_excerpt: fields.body_text_excerpt,
        embedding,
        kb_category: fields.kb_category,
        kb_status: fields.kb_status,
        kb_severity: fields.kb_severity,
        kb_salience: fields.kb_salience,
        kb_decay: fields.kb_decay,
        kb_supersedes: fields.kb_supersedes,
        kb_session: fields.kb_session,
        kb_summary: fields.kb_summary,
        kb_memory_type: fields.kb_memory_type,
        kb_source: fields.kb_source,
        kb_author: fields.kb_author,
        kb_source_kb: fields.kb_source_kb,
        kb_source_artifact: fields.kb_source_artifact,
        kb_source_anchor: fields.kb_source_anchor,
        prompt_size_bytes: fields.prompt_size_bytes,
        size_kb,
        js_loc: fields.js_loc.as_str().to_string(),
        css_loc: fields.css_loc.as_str().to_string(),
        svg_count: fields.svg_count,
        has_svg: fields.svg_count > 0,
        has_form: fields.has_form,
        has_canvas: fields.has_canvas,
        has_animation: fields.has_animation,
        has_details: fields.has_details,
        has_script: fields.has_script,
        has_drag: fields.has_drag,
        has_math: fields.has_math,
        mtime_unix,
        indexed_at_unix: unix_now(),
        // RA3 — a `kb-created` meta (memories) wins over filesystem btime, so a
        // memory's creation time (its decay basis) survives reindex. Non-memory
        // artifacts have no such meta and keep btime.
        created_unix: fields.kb_created.or(created_unix),
        table_count: fields.table_count,
        code_block_count: fields.code_block_count,
        word_count: fields.word_count,
        longread: fields.longread,
        tags_csv: {
            // v0.6 T1 — explicit `<meta name="kb-tags">` wins; fall back
            // to the path-derived heuristic so every artifact gets at
            // least one tag in the gallery without requiring authors
            // to retag their existing files.
            let mut t = fields.tags.clone();
            if t.is_empty() {
                t = crate::parser::path_derived_tags(&path.to_string_lossy());
            }
            t.join(",")
        },
        // v0.16 — persisted so the next daemon start can populate the
        // dedup cache from lance and skip embedding unchanged artifacts.
        content_hash: Some(content_hash.clone()),
        // N-track — task-list progress for the notes views; 0/0 for
        // artifacts without GFM task lists.
        task_done: Some(fields.task_done),
        task_total: Some(fields.task_total),
    };

    // GC-B7 — no storage write here anymore: `doc` + everything the finish
    // phase needs travels back to `process_ingest_batch` inside a
    // `PreparedDoc`, which batches the ACTUAL upsert across the whole
    // drained group (`flush_prepared_batch`). X2's snapshot-before-move
    // rationale still applies — these are captured from `doc` before it's
    // handed off, exactly as they were captured before the old singular
    // `upsert_doc` call.
    let enrich_category: Option<String> = doc.kb_category.clone();
    let enrich_mtime_unix = doc.mtime_unix;
    let seed_global = fields.kb_global;
    let seed_linked_kbs = fields.kb_linked_kbs.clone();

    Ok(Some(PreparedDoc {
        path: path.to_path_buf(),
        run_id: run_id.clone(),
        started: Instant::now(),
        change_kind,
        artifact_id,
        content_hash,
        rel_path,
        html: html.to_string(),
        raw: raw.to_string(),
        session_parse,
        doc,
        chunk_docs,
        enrich_category,
        enrich_mtime_unix,
        seed_global,
        seed_linked_kbs,
        embed_gated,
    }))
}

/// GC-B7 — phase 2 of the (former `index_file`) pipeline, run AFTER the
/// doc's row already exists in Lance (batched or singular — the caller
/// doesn't distinguish): chunk sync, enrichment hooks, anchor
/// re-resolution, the `artifact.indexed` event, and the dedup-cache
/// insert. Ends in the same `finish_run_tail(ok=true)` every other exit
/// path uses.
#[allow(clippy::too_many_arguments)]
async fn finish_indexed_doc(
    p: PreparedDoc,
    kb_name: &KbName,
    source_slug: &SourceSlug,
    source_root: &Path,
    storage: &StorageHandle,
    bus: &EventBus,
    quarantine_dir: &Path,
    review_dir: Option<&Path>,
    anchor_state: Arc<Mutex<HashMap<(String, String), crate::anchors::StaleAnchorEntry>>>,
    indexed_hashes: DedupCache,
    artifact_host_suffix: &str,
    versions_mode: crate::vcs::VersionsMode,
    metrics: &crate::metrics::PipelineMetrics,
    chunked: bool,
) {
    let PreparedDoc {
        path,
        run_id,
        started,
        change_kind,
        artifact_id,
        content_hash,
        rel_path,
        html,
        raw,
        session_parse,
        doc: _doc,
        chunk_docs,
        enrich_category,
        enrich_mtime_unix,
        seed_global,
        seed_linked_kbs,
        embed_gated,
    } = p;
    let path = path.as_path();
    let html = html.as_str();
    let raw = raw.as_str();

    // SQ5 — sync the doc's passage chunks (after the doc upsert, so the
    // row exists for chunk_vector_query's resolve step). Gate on `chunked`
    // (NOT on chunk_docs being non-empty): when chunking is on but a
    // chunk-embed failed (or the doc is body-less), chunk_docs is empty and
    // the empty replace clears the doc's STALE chunks — otherwise old
    // passages would linger and surface in search. Non-fatal: a failure
    // leaves search on the doc-level vector arm.
    if chunked {
        if let Err(e) = storage
            .upsert_chunks(artifact_id.as_str().to_string(), chunk_docs)
            .await
        {
            tracing::warn!(kb = %kb_name, error = %e, "upsert_chunks failed (non-fatal)");
        }
    }

    // v0.33 X2 — stable first-indexed timestamp (sqlite `doc_first_seen`).
    // INSERT OR IGNORE so reindex never rewrites it. Only on the full
    // success tail (this function) — never on the hot dedup-skip path.
    {
        let ts = chrono::Utc::now().timestamp();
        if let Err(e) = storage
            .first_seen_insert_ignore(artifact_id.as_str().to_string(), ts)
            .await
        {
            tracing::warn!(
                kb = %kb_name,
                artifact_id = %artifact_id.as_str(),
                error = %e,
                "first_seen_insert_ignore failed (non-fatal)"
            );
        }
    }

    // X2 — post-upsert enrichment. The three enrichers (session capture,
    // memory-link seed, edge recording) are now EnrichmentHook impls
    // (crate::enrich) run in registration order. Each is best-effort and
    // self-handles its own errors (tracing / record_failure), so behaviour
    // + emitted events stay byte-identical to the pre-X2 inline blocks; the
    // registry-level warn is a safety net for future hooks that return Err.
    // The next sessions-shaped feature is one new impl + one `default_hooks`
    // line instead of another inline branch here. Writes go through the
    // StorageHandle (single-writer-per-kb).
    {
        let enrich_ctx = crate::enrich::EnrichCtx {
            kb_name,
            source_slug,
            storage,
            bus,
            quarantine_dir,
            source_root,
            path,
            artifact_id: &artifact_id,
            rel_path: rel_path.as_str(),
            html,
            artifact_host_suffix,
            kb_category: enrich_category.as_deref(),
            mtime_unix: enrich_mtime_unix,
            // DCB W1.A (R4) — ONE wall-clock read per doc-finish, threaded
            // down so no hook reaches for a live clock itself.
            now_unix: now_unix(),
            seed_global,
            seed_linked_kbs: seed_linked_kbs.as_slice(),
            content_hash: content_hash.as_str(),
            raw_source: raw,
            versions_mode,
            session_parse: session_parse.as_ref(),
        };
        for hook in crate::enrich::default_hooks() {
            if !hook.interested(&enrich_ctx) {
                continue;
            }
            if let Err(e) = hook.enrich(&enrich_ctx).await {
                tracing::warn!(
                    kb = %kb_name,
                    hook = hook.name(),
                    path = %path.display(),
                    error = %e,
                    "enrichment hook returned error (skipped)",
                );
            }
        }
    }

    // Success: dismiss every open error row for this path. Pre-fix the
    // call was `clear_errors_for_path_hash(new_hash)` — it only cleared
    // rows for *other* hashes (i.e. the file content had to change for
    // the slate to be wiped). That left a stale-error footprint when a
    // failure was environmental (e.g. a lance OOM that fixed itself
    // after a config bump): the bytes were the same, so the old rows
    // lingered in the Errors tab even though the indexer was happily
    // re-indexing the file every reconcile. Successful indexing is
    // itself the proof that no open error applies — clear them all.
    //
    // 2026-08-21 ci-host incident hotfix: EXCEPT when this doc's embed was
    // quarantine-gated (`embed_gated`, `prepare_doc`). A gated doc still
    // reaches this success tail (it indexes fine — BM25/keyword-searchable),
    // so without this guard the auto-dismiss above would clear the very
    // error row the gate reads on the NEXT reconcile pass — un-gating the
    // doc, letting it take a real (OOM-repeating) embed attempt again, and
    // reproducing the original infinite loop through a side door. A gated
    // pass proves nothing about whether the failure condition is gone (the
    // embed was never attempted), so the row must survive untouched until a
    // genuine content change gives the doc a fresh content_hash.
    if !embed_gated {
        match storage.clear_errors_for_path(path.to_path_buf()).await {
            Ok(n) if n > 0 => {
                bus.emit(
                    "error.dismissed",
                    json!({
                        "kb": kb_name.as_str(),
                        "path": path.to_string_lossy(),
                        "count": n,
                        "reason": "auto-cleared on successful index",
                    }),
                );
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(
                kb = %kb_name,
                path = %path.display(),
                error = %e,
                "clear_errors_for_path failed (non-fatal)"
            ),
        }
    }

    // v0.3 G2 + v0.5 P4 — re-resolve open comment anchors against the
    // new HTML. Two events fire at the kb_core::review::Resolution
    // transitions, both keyed on the in-process anchor_state tracker:
    //   - missing → Stale  → insert key + emit `comment.anchor_stale`
    //   - Stale → Exact/Fuzzy → remove key + emit `comment.anchor_resolved`
    //   - missing → Exact/Fuzzy → no event (steady state)
    //   - Stale → Stale         → no event (already flagged)
    // Tests pass `review_dir = None` to skip the disk read entirely.
    if let Some(dir) = review_dir {
        // v0.7.1 H2 — migrate a pre-v0.7 review file (keyed on the old
        // content-hash id) to the path-based id, so comments authored
        // before the v0.7 id change keep loading. Fires only when the
        // artifact's current bytes still hash to the legacy stem; a
        // no-op otherwise, and idempotent once migrated.
        if let Err(e) = crate::review::migrate_legacy_id(dir, artifact_id.as_str(), &content_hash) {
            tracing::warn!(
                kb = %kb_name, path = %path.display(), error = %e,
                "failed to migrate legacy review file id"
            );
        }
        let review_path = dir.join(format!("{}.json", artifact_id.as_str()));
        if let Ok(Some(file)) = crate::review::load(&review_path) {
            // Set when the stale set gains or loses a key — drives the
            // single post-loop sidecar write (v0.7 P1).
            let mut anchor_state_changed = false;
            // ONE parse of the html shared across every open comment's
            // re-resolution (the `_with` variant fills the slot on the first
            // DOM-needing anchor; File anchors never parse). `scraper::Html`
            // is !Send but this loop has no `.await`, and the slot drops at
            // the end of the enclosing `if let` block before any does.
            let mut anchor_dom: Option<scraper::Html> = None;
            // M2 / memo R2 — the SAME sharing trick, for `memory-session`
            // captures: a `SessionView` built once (`session_view_for_
            // capture_html`) and reused across every comment on this
            // document, so a turn-anchored review thread doesn't re-run the
            // whole session-view/1 engine per comment. `is_memory_session`
            // gates the WHOLE resolution rule per doc (indexer.rs's
            // `enrich_category` is the same value the enrich hooks already
            // gate on) — see `sessions::view::resolve_capture_anchor` for
            // why the raw-HTML resolver can never find a `t-<uuid12>`/
            // `#ses-outcome` id or rendered prose in the escaped `<pre>`
            // bytes it parses.
            let is_memory_session =
                enrich_category.as_deref() == Some(crate::sessions::MEMORY_SESSION_CATEGORY);
            let mut session_anchor_view: Option<crate::sessions::view::SessionView> = None;
            for comment in file.comments.iter().filter(|c| c.is_open()) {
                let key = (artifact_id.as_str().to_string(), comment.id.clone());
                let resolution = if is_memory_session {
                    let view = session_anchor_view.get_or_insert_with(|| {
                        crate::sessions::view::session_view_for_capture_html(html)
                    });
                    crate::sessions::view::resolve_capture_anchor(view, html, &comment.anchor)
                } else {
                    crate::review::fuzzy_resolve_anchor_with(&mut anchor_dom, html, &comment.anchor)
                };
                let was_stale = anchor_state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .contains_key(&key);
                match (&resolution, was_stale) {
                    (crate::review::Resolution::Stale, false) => {
                        // #4 — kind = anchor scope (file/chapter/section/
                        // selection); score = 0 for now (Resolution::Stale
                        // is a unit variant so the resolver doesn't yet
                        // report the best-tried similarity). The sidecar
                        // schema (v3) is ready to round-trip a richer
                        // value when the resolver grows that signal.
                        let kind = comment.anchor.scope_name().to_string();
                        let meta = crate::anchors::StaleAnchorEntry {
                            anchor_kind: kind.clone(),
                            fuzzy_score: 0.0,
                        };
                        anchor_state
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(key, meta);
                        anchor_state_changed = true;
                        bus.emit(
                            "comment.anchor_stale",
                            json!({
                                "kb": kb_name.as_str(),
                                "artifact_id": artifact_id.as_str(),
                                "comment_id": comment.id,
                                "anchor_kind": kind,
                                "fuzzy_score": 0.0,
                                // Track U — carry the source-relative path
                                // so the SPA's stale-anchors dashboard can
                                // build the `/a/<kb>/<path>` deep-link from
                                // a live event (not just the cold load).
                                "source_relative": rel_path.as_str(),
                            }),
                        );
                    }
                    (crate::review::Resolution::Stale, true) => {
                        // Already flagged stale on a prior reindex; no
                        // event (the SPA already painted the badge).
                    }
                    (
                        crate::review::Resolution::Exact(_)
                        | crate::review::Resolution::Fuzzy(_, _),
                        true,
                    ) => {
                        anchor_state
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .remove(&key);
                        anchor_state_changed = true;
                        let score = match &resolution {
                            crate::review::Resolution::Fuzzy(_, s) => Some(*s),
                            _ => None,
                        };
                        bus.emit(
                            "comment.anchor_resolved",
                            json!({
                                "kb": kb_name.as_str(),
                                "artifact_id": artifact_id.as_str(),
                                "comment_id": comment.id,
                                "score": score,
                            }),
                        );
                    }
                    _ => {}
                }
            }
            // R5 — drop stale-anchor keys for comments that no longer
            // exist in the file (deleted via the DELETE endpoint). The
            // open-comment loop above only visits live open comments, so a
            // deleted comment's key would otherwise be re-persisted to the
            // sidecar on every reindex — resurfacing a phantom on the
            // stale-anchor dashboard after the DELETE handler pruned the
            // on-disk entry. Only this artifact's keys are pruned; a merely
            // *resolved* comment is still present in `file.comments`, so it
            // is not affected here.
            {
                let live: std::collections::HashSet<&str> =
                    file.comments.iter().map(|c| c.id.as_str()).collect();
                let mut guard = anchor_state.lock().unwrap_or_else(|e| e.into_inner());
                let before = guard.len();
                guard.retain(|(aid, cid), _| {
                    aid != artifact_id.as_str() || live.contains(cid.as_str())
                });
                if guard.len() != before {
                    anchor_state_changed = true;
                }
            }
            // Persist the stale set so it survives a daemon restart.
            // One write per artifact-reindex-that-changed-something —
            // the file is tiny and this only fires for artifacts that
            // actually have comments.
            if anchor_state_changed {
                let sidecar = crate::anchors::sidecar_path(dir);
                // The whole map is this kb's (the indexer is per-kb), so
                // snapshot every entry — including the v3 metadata.
                let snapshot: HashMap<(String, String), crate::anchors::StaleAnchorEntry> =
                    anchor_state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect();
                if let Err(e) = crate::anchors::save(&sidecar, &snapshot) {
                    tracing::warn!(
                        kb = %kb_name,
                        error = %e,
                        "failed to persist anchor-stale sidecar"
                    );
                }
            }
        }
    }

    bus.emit(
        "artifact.indexed",
        json!({
            "artifact_id": artifact_id.as_str(),
            "kb": kb_name.as_str(),
            "path": path.to_string_lossy(),
            "hash": content_hash.clone(),
            "mtime": enrich_mtime_unix,
            "change_kind": change_kind,
        }),
    );

    // Populate the dedup cache. Done at the success tail so a failed
    // parse/embed/upsert/edges path doesn't poison the cache — the next
    // event for this artifact retries cleanly through the full pipeline.
    // v0.24 SC1 — the entry carries the mtime that was just committed
    // (`doc.mtime_unix`, snapshotted as `enrich_mtime_unix`), so a later
    // duplicate emission of this unchanged file compares disk == stored
    // and skips the `touch_mtime` heal entirely.
    indexed_hashes
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(
            artifact_id.as_str().to_string(),
            IndexedMeta {
                content_hash,
                mtime_unix: Some(enrich_mtime_unix),
            },
        );

    finish_run_tail(bus, storage, kb_name, metrics, &run_id, path, started, true).await;
}

// X2 — `pub(crate)` so the edge-record EnrichmentHook (crate::enrich) can
// surface an edge-write failure to the Errors tab exactly as the inline
// block did before the refactor.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn record_failure(
    kb_name: &KbName,
    source_slug: &SourceSlug,
    storage: &StorageHandle,
    bus: &EventBus,
    quarantine_dir: &Path,
    path: &Path,
    kind: &str,
    message: String,
    content_hash: Option<String>,
) -> Result<(), String> {
    let _id = storage
        .record_error(
            kind.to_string(),
            source_slug.clone(),
            path.to_path_buf(),
            message.clone(),
            content_hash.clone(),
            unix_now(),
        )
        .await
        .map_err(|e| format!("record_error: {e}"))?;

    bus.emit(
        "error",
        json!({
            "id": _id.as_str(),
            "kind": kind,
            "kb": kb_name.as_str(),
            "path": path.to_string_lossy(),
            "msg": message,
        }),
    );

    // Quarantine if retry_count crosses the threshold (only meaningful
    // when content_hash is set — IO errors without hash don't accumulate).
    if let Some(hash) = content_hash {
        let retries = storage
            .retry_count_for_path_hash(path.to_path_buf(), hash)
            .await
            .unwrap_or(0);
        if retries + 1 >= QUARANTINE_THRESHOLD {
            quarantine(quarantine_dir, path, &message);
        }
    }

    Err(message)
}

#[allow(clippy::too_many_arguments)]
async fn process_delete(
    kb_name: &KbName,
    source_slug: &SourceSlug,
    source_root: &Path,
    storage: &StorageHandle,
    bus: &EventBus,
    path: &Path,
    indexed_hashes: DedupCache,
    review_dir: Option<&Path>,
    review_lock: Option<&tokio::sync::Mutex<()>>,
    // X1 — when set (reconcile's exclusion-shaped delete for a de-mapped file
    // still on disk), keep the `.review` sidecar + reading history; the index
    // row still goes. `false` (a genuine delete) runs the Full cascade.
    keep_user_data: bool,
) {
    process_delete_inner(
        kb_name,
        source_slug,
        source_root,
        storage,
        bus,
        path,
        indexed_hashes,
        review_dir,
        review_lock,
        keep_user_data,
        Some(crate::relocate::shared_pending()),
    )
    .await;
}

/// F3a test hook — same as the indexer delete path, with an injectable
/// pending-moves guard.
#[allow(clippy::too_many_arguments)]
pub async fn process_delete_for_test(
    kb_name: &KbName,
    source_slug: &SourceSlug,
    source_root: &Path,
    storage: &StorageHandle,
    bus: &EventBus,
    path: &Path,
    indexed_hashes: Arc<Mutex<HashMap<String, IndexedMeta>>>,
    review_dir: Option<&Path>,
    review_lock: Option<&tokio::sync::Mutex<()>>,
    keep_user_data: bool,
    pending: Option<Arc<crate::relocate::PendingMoves>>,
) {
    process_delete_inner(
        kb_name,
        source_slug,
        source_root,
        storage,
        bus,
        path,
        indexed_hashes,
        review_dir,
        review_lock,
        keep_user_data,
        pending,
    )
    .await;
}

/// One entry in the shared [`DedupCache`]: content hash + stored mtime.
///
/// Public so relocate (and its tests) can seed / rekey the real cache type
/// rather than a parallel id→hash map. Content bytes are unchanged by a
/// rename and rename(2) preserves mtime on Linux, so both fields stay valid
/// across a relocate rekey.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexedMeta {
    pub content_hash: String,
    pub mtime_unix: Option<i64>,
}

#[allow(clippy::too_many_arguments)]
async fn process_delete_inner(
    kb_name: &KbName,
    source_slug: &SourceSlug,
    source_root: &Path,
    storage: &StorageHandle,
    bus: &EventBus,
    path: &Path,
    indexed_hashes: DedupCache,
    review_dir: Option<&Path>,
    review_lock: Option<&tokio::sync::Mutex<()>>,
    // X1 — when set (reconcile's exclusion-shaped delete for a de-mapped file
    // still on disk), keep the `.review` sidecar + reading history; the index
    // row still goes. `false` (a genuine delete) runs the Full cascade.
    keep_user_data: bool,
    pending_moves: Option<Arc<crate::relocate::PendingMoves>>,
) {
    // Match the CANONICAL path `index_file` stored (invariant #24): on a
    // symlinked source root the raw event path won't equal the stored row.
    // The file usually still exists for watcher events (canonicalises
    // cleanly); reconcile already emits canonical paths, and a genuinely
    // vanished file falls back to its raw form unchanged.
    let del_path = crate::paths::canonical_abs(path);
    // v0.7.1 P2 — derive the stable path-based id up front (pure — it doesn't
    // need the file to exist). The file is gone, but its path string is
    // intact, so `doc_rel_path`'s raw-prefix fallback still derives the right
    // rel path. Mirrors `index_file`'s id derivation. R2 needs the id BEFORE
    // the delete now (the sqlite cascade keys on it).
    let rel_path = crate::paths::doc_rel_path(&path.to_string_lossy(), source_root);
    let artifact_id = if rel_path.is_empty() {
        ArtifactId::from_path(&path.to_string_lossy())
    } else {
        ArtifactId::from_path(&rel_path)
    };

    // F3a — relocate race guard (in-memory pending + durable moves table).
    // A Debounced Deleted for the old path of a just-renamed artifact must
    // NOT cascade the migrated state. Checked before the try_exists recreate
    // gate so a completed rename (old path gone) is still suppressed.
    if crate::relocate::should_suppress_delete(storage, pending_moves.as_deref(), path, source_root)
        .await
    {
        tracing::info!(
            kb = %kb_name,
            path = %path.display(),
            artifact = %artifact_id.as_str(),
            "delete event suppressed: path is a pending/recent relocate source",
        );
        return;
    }

    // W0.8 — a Full cascade destroys the `.review` sidecar + attachments +
    // reading history (invariant #6), so it must never fire on a signal that
    // only LOOKS like a deletion. `keep_user_data=false` covers two shapes
    // that share no marker distinguishing them: reconcile's walk-confirmed
    // "the file is truly gone" delete, AND every live-watcher `Deleted` event
    // — including the Remove(+Create) pair `notify`/`notify-debouncer-full`
    // synthesizes for an atomic temp-file+rename replace, or for a burst of
    // rapid saves that changes the path's underlying file identity within one
    // debounce window. Root cause of the 2026-07-15 (13 Edit-tool saves to
    // corpus-a/dogfood-test-plan-2026-07.html) and 2026-07-17 (burst
    // edits + a `just ship` corpus rebuild on research/read-first-ide.html,
    // 7 resolved threads lost) incidents: this Deleted item can sit behind a
    // backlog on the back-pressured ingest channel (invariant #17) before
    // `process_delete` actually runs it, and by then the file is back — yet
    // the pre-fix code ran the Full cascade unconditionally on the EVENT
    // TYPE alone, never re-checking reality. Reconcile's own delete pass
    // already re-stats right before deciding Full vs KeepUserData
    // (`path.try_exists()` in `reconcile`); mirror that discipline HERE, at
    // the point of action, so it covers both origins: if the source exists
    // again by the time we're about to act, this was a transient churn, not
    // a real delete — skip the cascade entirely (no lance drop, no sidecar
    // touch, no `artifact.removed`) and let the trailing Created/Modified
    // event (already queued, or on its way) re-affirm the still-live row.
    // `try_exists` failing (e.g. a permission race) errs toward "still
    // there" — never destroy user data on an ambiguous stat.
    if !keep_user_data && path.try_exists().unwrap_or(true) {
        tracing::info!(
            kb = %kb_name,
            path = %path.display(),
            artifact = %artifact_id.as_str(),
            "delete event raced a recreate — source file exists again; \
             skipping the cascade so the review sidecar survives",
        );
        return;
    }

    // R2 — ONE systematic cascade replaces the pre-R2 ad-hoc sequence
    // (lance-by-path, then best-effort sessions/snapshots/memory_links). A
    // lance failure is a hard delete failure (record + return, exactly as
    // before, so `artifact.removed` doesn't fire on a row that's still there);
    // the sqlite dependents go atomically and the `.review` sidecar +
    // attachments are reaped under the comment lock.
    let mode = if keep_user_data {
        crate::cascade::CascadeMode::KeepUserData
    } else {
        crate::cascade::CascadeMode::Full
    };
    let report = match crate::cascade::delete_artifact(
        storage,
        &artifact_id,
        &del_path,
        mode,
        review_dir,
        review_lock,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(kb = %kb_name, path = %path.display(), error = %e, "delete failed");
            let _ = storage
                .record_error(
                    "storage".into(),
                    source_slug.clone(),
                    path.to_path_buf(),
                    format!("delete: {e}"),
                    None,
                    unix_now(),
                )
                .await;
            return;
        }
    };
    // Invalidate the content-hash dedup cache for this id. Removing the
    // entry is what makes a delete-then-recreate-with-same-bytes pair
    // re-index: since v0.16 the dedup gate applies to BOTH Created and
    // Modified, so without this removal the recreate's hash would match the
    // cached value and the gate would (wrongly) short-circuit it.
    indexed_hashes
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(artifact_id.as_str());
    bus.emit(
        "artifact.removed",
        json!({
            "artifact_id": artifact_id.as_str(),
            "kb": kb_name.as_str(),
            "path": path.to_string_lossy(),
        }),
    );
    // v0.14 S2 — the cascade dropped any matching sessions enrichment row; fire
    // `session.deleted` only when it actually removed one (preserves the
    // pre-R2 SSE, which only emitted for real memory-session artifacts).
    if report.sessions_removed > 0 {
        bus.emit(
            "session.deleted",
            json!({
                "kb": kb_name.as_str(),
                "artifact_id": artifact_id.as_str(),
            }),
        );
    }
    // R2 — the cascade deliberately does NOT prune this artifact's reading-list
    // entries: they survive and render as tombstones (derived at read time from
    // the now-missing lance doc), and the SPA already learns of the deletion via
    // `artifact.removed` above, so no `list.updated` is needed.
}

fn quarantine(quarantine_dir: &Path, path: &Path, error_msg: &str) {
    // Best-effort, but NOT silent: this is a recovery mechanism, and the
    // pre-fix `let _ =` swallowed every failure then logged the success
    // line anyway — telling the operator a recovery copy existed when the
    // copy had failed (full disk, permissions, vanished source).
    if let Err(e) = std::fs::create_dir_all(quarantine_dir) {
        tracing::warn!(
            dir = %quarantine_dir.display(),
            error = %e,
            "quarantine dir creation failed; artifact NOT quarantined",
        );
        return;
    }
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("artifact");
    let dst = quarantine_dir.join(format!("{stem}.html"));
    let sidecar = quarantine_dir.join(format!("{stem}.error.txt"));
    let copied = match std::fs::copy(path, &dst) {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!(
                src = %path.display(),
                dst = %dst.display(),
                error = %e,
                "quarantine copy failed; only the error sidecar may exist",
            );
            false
        }
    };
    if let Err(e) = std::fs::write(
        &sidecar,
        format!("path: {}\nerror: {}\n", path.display(), error_msg),
    ) {
        tracing::warn!(
            sidecar = %sidecar.display(),
            error = %e,
            "quarantine error-sidecar write failed",
        );
    }
    if copied {
        tracing::warn!(quarantine = %dst.display(), "artifact quarantined after consecutive failures");
    }
}

// X2 — `pub(crate)` so the memory-link-seed EnrichmentHook (crate::enrich)
// stamps the same `now` the inline block used.
pub(crate) fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::StorageActor;
    use crate::types::KbName;
    use std::time::Duration;
    use tempfile::tempdir;
    use tokio::time::timeout;

    // --- SQ5 — per-doc chunk-count cap --------------------------------------

    fn fake_chunks(n: u32) -> Vec<crate::chunk::Chunk> {
        (0..n)
            .map(|idx| crate::chunk::Chunk {
                idx,
                text: format!("chunk {idx}"),
            })
            .collect()
    }

    #[test]
    fn cap_chunks_to_is_a_noop_at_and_under_the_cap() {
        let chunks = fake_chunks(5);
        let (kept, total) = cap_chunks_to(chunks.clone(), 5);
        assert_eq!(total, 5);
        assert_eq!(kept, chunks);

        let chunks = fake_chunks(3);
        let (kept, total) = cap_chunks_to(chunks.clone(), 5);
        assert_eq!(total, 3, "total reports the ORIGINAL count, not the cap");
        assert_eq!(kept, chunks, "under-cap list is untouched");
    }

    /// 2026-08-21 ci-host incident, defect 2 — a doc whose chunk list exceeds
    /// the cap gets exactly `cap` rows back, the FIRST ones in reading
    /// order (chunk-0 — the title/headings chunk in the real pipeline —
    /// survives). `total` still reports the ORIGINAL (uncapped) count so
    /// the caller can tell truncation happened and log it honestly.
    #[test]
    fn cap_chunks_to_truncates_to_the_first_n_and_reports_the_original_total() {
        let chunks = fake_chunks(20);
        let (kept, total) = cap_chunks_to(chunks, 7);
        assert_eq!(
            total, 20,
            "must report the ORIGINAL count for the event/log"
        );
        assert_eq!(kept.len(), 7, "MAX_CHUNKS_PER_DOC=7 (test-scale) rows kept");
        assert_eq!(
            kept,
            fake_chunks(7),
            "kept chunks are the FIRST 7 in reading order — idx 0 (chunk-0) survives"
        );
    }

    #[test]
    fn cap_chunks_to_handles_an_empty_list() {
        let (kept, total) = cap_chunks_to(Vec::new(), MAX_CHUNKS_PER_DOC);
        assert_eq!(total, 0);
        assert!(kept.is_empty());
    }

    // --- Query-priority lane: indexer-side yield ---------------------------

    #[tokio::test]
    async fn yield_to_pending_queries_is_a_noop_when_none_pending() {
        // Steady state (no query contending) must add ~zero latency to the
        // indexer's embed path.
        let started = Instant::now();
        yield_to_pending_queries("test-yield-none").await;
        assert!(
            started.elapsed() < Duration::from_millis(50),
            "no-op path took {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn yield_to_pending_queries_returns_once_query_drains() {
        // A held QueryLaneGuard keeps the count > 0; dropping it (a query
        // finishing) lets the indexer proceed — well before the safety cap.
        let model = "test-yield-drain";
        let g = crate::embed::QueryLaneGuard::enter(model);
        let dropper = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(40)).await;
            drop(g); // the "query" completes → pending count returns to 0
        });
        let started = Instant::now();
        yield_to_pending_queries(model).await;
        let elapsed = started.elapsed();
        dropper.await.unwrap();
        assert!(
            elapsed >= Duration::from_millis(30),
            "should have waited for the pending query: {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_millis(450),
            "should return once the query drained, well before the 500ms cap: {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn yield_to_pending_queries_is_bounded_by_the_cap() {
        // A query that never drains must NOT starve indexing forever — the
        // yield is capped so the indexer always makes progress.
        let model = "test-yield-cap";
        let _g = crate::embed::QueryLaneGuard::enter(model);
        let started = Instant::now();
        yield_to_pending_queries(model).await;
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_millis(500),
            "should yield up to the cap: {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_millis(2000),
            "but must return at the cap, not hang: {elapsed:?}"
        );
    }

    #[test]
    fn walk_emit_modify_finds_html_and_markdown_recursively() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("a.html"), "<html></html>").unwrap();
        std::fs::write(root.join("b.htm"), "<html></html>").unwrap();
        std::fs::write(root.join("notes.md"), "# md").unwrap();
        std::fs::write(root.join("skip.txt"), "ignore").unwrap();
        std::fs::create_dir_all(root.join("sub/deep")).unwrap();
        std::fs::write(root.join("sub/c.html"), "<html></html>").unwrap();
        std::fs::write(root.join("sub/deep/d.HTML"), "<html></html>").unwrap();

        let bus = EventBus::default();
        let kb = KbName::new("smoke").unwrap();
        let mut rx = bus.subscribe();

        let outcome = walk_emit_modify(&bus, &kb, root, &[], false, None);
        assert_eq!(
            outcome.emitted, 5,
            "4 html/htm + 1 md indexed; the .txt skipped"
        );

        // Drain the bus and confirm every envelope is watch.modify with
        // the right kb tag.
        let mut paths: Vec<String> = Vec::new();
        for _ in 0..outcome.emitted {
            let env = rx.try_recv().expect("envelope present");
            assert_eq!(env.type_, "watch.modify");
            assert_eq!(env.payload["kb"].as_str(), Some("smoke"));
            paths.push(env.payload["path"].as_str().unwrap().to_string());
        }
        paths.sort();
        assert!(paths.iter().any(|p| p.ends_with("a.html")));
        assert!(
            paths.iter().any(|p| p.ends_with("notes.md")),
            "markdown indexed"
        );
        assert!(
            !paths.iter().any(|p| p.ends_with("skip.txt")),
            "txt skipped"
        );
    }

    #[test]
    fn walk_emit_modify_returns_zero_for_empty_or_missing_root() {
        let bus = EventBus::default();
        let kb = KbName::new("smoke").unwrap();
        let tmp = tempdir().unwrap();
        // Empty dir.
        assert_eq!(
            walk_emit_modify(&bus, &kb, tmp.path(), &[], false, None).emitted,
            0
        );
        // Non-existent dir — walkdir returns no entries (silently).
        let missing = tmp.path().join("does-not-exist");
        assert_eq!(
            walk_emit_modify(&bus, &kb, &missing, &[], false, None).emitted,
            0
        );
    }

    #[test]
    fn walk_emit_modify_honours_skip_patterns() {
        // K1 regression: the reconciler used to ignore the kb's
        // skip_patterns and silently re-emit `watch.modify` envelopes
        // for files the watcher correctly excluded. After the fix the
        // walk filters via `watcher::is_skipped`, matching the live path.
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("templates")).unwrap();
        std::fs::create_dir_all(root.join("drafts")).unwrap();
        std::fs::write(root.join("keep.html"), "<html></html>").unwrap();
        std::fs::write(root.join("templates/skip-me.html"), "<html></html>").unwrap();
        std::fs::write(root.join("drafts/wip.html"), "<html></html>").unwrap();

        let bus = EventBus::default();
        let kb = KbName::new("smoke").unwrap();
        let skips = vec!["templates/**".to_string(), "drafts/**".to_string()];

        let outcome = walk_emit_modify(&bus, &kb, root, &skips, false, None);
        assert_eq!(
            outcome.emitted, 1,
            "only keep.html should be walked; got {} paths: {:?}",
            outcome.emitted, outcome.seen_paths
        );
        assert!(outcome.seen_paths.iter().any(|p| p.ends_with("keep.html")));
        // Patterns hit:
        assert!(!outcome
            .seen_paths
            .iter()
            .any(|p| p.to_string_lossy().contains("templates/")));
        assert!(!outcome
            .seen_paths
            .iter()
            .any(|p| p.to_string_lossy().contains("drafts/")));
    }

    #[test]
    fn walk_emit_modify_skips_unchanged_mtime_but_still_sees_them() {
        // G5 producer-side dedup: on the !force safety-net pass, a file whose
        // on-disk mtime matches the stored mtime must NOT re-emit a
        // watch.modify — but it MUST still appear in seen_paths so the delete
        // pass doesn't mistake it for vanished. A file with a stale stored
        // mtime (a real edit) DOES re-emit; force=true ignores the map.
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let unchanged = root.join("unchanged.html");
        let changed = root.join("changed.html");
        std::fs::write(&unchanged, "<html></html>").unwrap();
        std::fs::write(&changed, "<html></html>").unwrap();

        let disk_mtime = |p: &Path| -> i64 {
            std::fs::metadata(p)
                .unwrap()
                .modified()
                .unwrap()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64
        };
        let mut known: HashMap<PathBuf, i64> = HashMap::new();
        known.insert(unchanged.clone(), disk_mtime(&unchanged)); // matches → skip
        known.insert(changed.clone(), disk_mtime(&changed) - 100); // stale → emit

        let bus = EventBus::default();
        let kb = KbName::new("smoke").unwrap();

        let outcome = walk_emit_modify(&bus, &kb, root, &[], false, Some(&known));
        assert_eq!(
            outcome.emitted, 1,
            "only the stale-mtime file should re-emit; got {:?}",
            outcome.seen_paths
        );
        // Both files are SEEN (existence tracked for the delete pass)...
        assert_eq!(outcome.seen_paths.len(), 2, "both files must be seen");
        assert!(outcome.seen_paths.contains(&unchanged));
        assert!(outcome.seen_paths.contains(&changed));

        // force=true ignores the mtime map entirely and re-emits both.
        let bus2 = EventBus::default();
        let forced = walk_emit_modify(&bus2, &kb, root, &[], true, Some(&known));
        assert_eq!(
            forced.emitted, 2,
            "force=true must emit every file regardless of stored mtime"
        );
    }

    #[tokio::test]
    async fn reconcile_emits_modify_for_file_not_yet_indexed() {
        // Simulates the bug class the reconciler is meant to fix: a file
        // appeared on disk without the watcher ever delivering a
        // `watch.create`. Calling `walk_emit_modify` after the fact
        // emits `watch.modify` and the indexer treats it like any other
        // modify — indexes the file. This is the seamless-recovery path.
        let (bus, storage, kb, slug, tmp) = setup().await;
        let quarantine = tmp.path().join("quarantine");
        let indexer_rx = bus.subscribe();
        let bus_for_indexer = bus.clone();
        let storage_for_indexer = storage.clone();
        let kb_for_indexer = kb.clone();
        let slug_for_indexer = slug.clone();
        let quarantine_for_indexer = quarantine.clone();
        let _indexer = tokio::spawn(async move {
            run(
                kb_for_indexer,
                slug_for_indexer,
                storage_for_indexer,
                bus_for_indexer,
                quarantine_for_indexer,
                indexer_rx,
                None,
                None,
                crate::iframe::DEFAULT_HOST_SUFFIX.to_string(),
                crate::vcs::VersionsMode::Off,
                0,
                Vec::new(),
            )
            .await
        });
        let mut rx = bus.subscribe();

        // Write a file directly to disk — no watcher, no synthetic
        // create envelope. The reconciler's walk is the only signal.
        let html_path = tmp.path().join("missed.html");
        std::fs::write(
            &html_path,
            "<html><title>Missed</title><body>x</body></html>",
        )
        .unwrap();

        let emitted = walk_emit_modify(&bus, &kb, tmp.path(), &[], false, None).emitted;
        assert_eq!(emitted, 1);

        // The indexer should produce an artifact.indexed for the missed
        // file within a reasonable window.
        let saw_indexed = timeout(Duration::from_secs(5), async {
            loop {
                match rx.recv().await {
                    Ok(env) if env.type_ == "artifact.indexed" => {
                        let path = env.payload["path"].as_str().unwrap_or("");
                        if path.ends_with("missed.html") {
                            return true;
                        }
                    }
                    Ok(_) => continue,
                    Err(_) => return false,
                }
            }
        })
        .await
        .unwrap_or(false);
        assert!(
            saw_indexed,
            "reconciler emit → indexer should index the file"
        );
    }

    #[tokio::test]
    async fn reconcile_emits_synthetic_delete_for_vanished_file() {
        // K2 regression: the W1 reconciler used to call walk_emit_modify
        // only — re-asserting existence but never propagating deletes.
        // A file removed from disk left a phantom row in lance whenever
        // inotify dropped the Remove event (queue overflow being the
        // canonical case W1 was meant to handle).
        let (bus, storage, kb, slug, tmp) = setup().await;
        let quarantine = tmp.path().join("quarantine");
        let indexer_rx = bus.subscribe();
        let bus_for_indexer = bus.clone();
        let storage_for_indexer = storage.clone();
        let kb_for_indexer = kb.clone();
        let slug_for_indexer = slug.clone();
        let quarantine_for_indexer = quarantine.clone();
        let _indexer = tokio::spawn(async move {
            run(
                kb_for_indexer,
                slug_for_indexer,
                storage_for_indexer,
                bus_for_indexer,
                quarantine_for_indexer,
                indexer_rx,
                None,
                None,
                crate::iframe::DEFAULT_HOST_SUFFIX.to_string(),
                crate::vcs::VersionsMode::Off,
                0,
                Vec::new(),
            )
            .await
        });

        // Plant a file, let it index, then unlink while the watcher is
        // unaware. The reconcile() call must notice and emit synthetic
        // watch.delete; the indexer then removes the row.
        let html_path = tmp.path().join("vanishing.html");
        std::fs::write(
            &html_path,
            "<html><title>Vanishing</title><body>x</body></html>",
        )
        .unwrap();

        // Drive an initial indexing via reconcile (it emits modify + the
        // indexer picks it up).
        let mut rx = bus.subscribe();
        // reconcile pushes to the sink's mpsc AND mirrors watch.* to the bus;
        // the spawned indexer reads the bus via the run() bridge wrapper, so
        // the mirror drives it. Keep _rx1 alive so blocking_send buffers
        // rather than erroring (the buffered copies are unused here).
        let (tx1, _rx1) = mpsc::channel(INGEST_QUEUE_CAPACITY);
        let _ = reconcile(
            IngestSink::new(tx1, bus.clone(), kb.clone()),
            storage.clone(),
            tmp.path().to_path_buf(),
            Vec::new(),
        )
        .await;

        let saw_indexed = timeout(Duration::from_secs(5), async {
            loop {
                match rx.recv().await {
                    Ok(env) if env.type_ == "artifact.indexed" => {
                        let path = env.payload["path"].as_str().unwrap_or("");
                        if path.ends_with("vanishing.html") {
                            return true;
                        }
                    }
                    Ok(_) => continue,
                    Err(_) => return false,
                }
            }
        })
        .await
        .unwrap_or(false);
        assert!(saw_indexed, "initial reconcile should index the file");

        // Now delete the file behind the watcher's back, then reconcile
        // again — must emit a synthetic watch.delete.
        std::fs::remove_file(&html_path).unwrap();
        let mut rx2 = bus.subscribe();
        let (tx2, _rx2) = mpsc::channel(INGEST_QUEUE_CAPACITY);
        let summary = reconcile(
            IngestSink::new(tx2, bus.clone(), kb.clone()),
            storage.clone(),
            tmp.path().to_path_buf(),
            Vec::new(),
        )
        .await;
        assert_eq!(summary.files_walked, 0, "no on-disk files left");
        assert_eq!(
            summary.deletes_emitted, 1,
            "vanished file must produce one synthetic delete"
        );

        let saw_delete = timeout(Duration::from_secs(3), async {
            loop {
                match rx2.recv().await {
                    Ok(env) if env.type_ == "watch.delete" => {
                        let path = env.payload["path"].as_str().unwrap_or("");
                        if path.ends_with("vanishing.html") {
                            return true;
                        }
                    }
                    Ok(_) => continue,
                    Err(_) => return false,
                }
            }
        })
        .await
        .unwrap_or(false);
        assert!(
            saw_delete,
            "reconcile must emit a synthetic watch.delete for the unlinked file"
        );
    }

    // Portability regression (P2): when the source root is a SYMLINK to the
    // real corpus dir — a symlinked source root, reproduced
    // deterministically here — reconcile must
    // still detect a vanished file. Before the fix it stored a raw `Doc.path`
    // but compared against the canonicalised root, so `starts_with` failed and
    // 0 deletes were emitted.
    // invariant:27 canonical-root
    #[cfg(unix)]
    #[tokio::test]
    async fn reconcile_emits_delete_through_a_symlinked_source_root() {
        let (bus, storage, kb, slug, tmp) = setup().await;

        // real_corpus/ holds the file; link_corpus -> real_corpus is the
        // path we reconcile against (the "raw, symlinked root"). Before the
        // fix the indexed path stayed raw (link_corpus/…) while reconcile
        // canonicalised the root (real_corpus/…), so the prefix check failed.
        let real = tmp.path().join("real_corpus");
        std::fs::create_dir_all(&real).unwrap();
        let link = tmp.path().join("link_corpus");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let quarantine = tmp.path().join("quarantine");
        let indexer_rx = bus.subscribe();
        let _indexer = tokio::spawn(run(
            kb.clone(),
            slug.clone(),
            storage.clone(),
            bus.clone(),
            quarantine,
            indexer_rx,
            None,
            None,
            crate::iframe::DEFAULT_HOST_SUFFIX.to_string(),
            crate::vcs::VersionsMode::Off,
            0,
            Vec::new(),
        ));

        let html_path = link.join("page.html");
        std::fs::write(
            &html_path,
            "<html><title>Linked</title><body>x</body></html>",
        )
        .unwrap();

        let mut rx = bus.subscribe();
        let (tx1, _rx1) = mpsc::channel(INGEST_QUEUE_CAPACITY);
        let _ = reconcile(
            IngestSink::new(tx1, bus.clone(), kb.clone()),
            storage.clone(),
            link.clone(),
            Vec::new(),
        )
        .await;
        let saw_indexed = timeout(Duration::from_secs(5), async {
            loop {
                match rx.recv().await {
                    Ok(env) if env.type_ == "artifact.indexed" => {
                        if env.payload["path"]
                            .as_str()
                            .unwrap_or("")
                            .ends_with("page.html")
                        {
                            return true;
                        }
                    }
                    Ok(_) => continue,
                    Err(_) => return false,
                }
            }
        })
        .await
        .unwrap_or(false);
        assert!(saw_indexed, "file under a symlinked root should index");

        std::fs::remove_file(&html_path).unwrap();
        let (tx2, _rx2) = mpsc::channel(INGEST_QUEUE_CAPACITY);
        let summary = reconcile(
            IngestSink::new(tx2, bus.clone(), kb.clone()),
            storage.clone(),
            link.clone(),
            Vec::new(),
        )
        .await;
        assert_eq!(
            summary.deletes_emitted, 1,
            "vanished file under a symlinked root must still emit one delete"
        );
    }

    // Perf regression: reconcile pulls the narrow `(path, mtime_unix)`
    // projection (`list_reconcile_rows`) to build the producer-side dedup
    // map. This pins that the mtime half of the tuple still drives the G5
    // dedup end-to-end — an unchanged file must NOT re-emit `watch.modify`
    // on a second reconcile pass (if the projection dropped/misread mtime,
    // the map would be empty and every pass would re-emit).
    #[tokio::test]
    async fn reconcile_dedups_unchanged_file_via_narrow_mtime_projection() {
        let (bus, storage, kb, slug, tmp) = setup().await;
        let quarantine = tmp.path().join("quarantine");
        let indexer_rx = bus.subscribe();
        let _indexer = tokio::spawn(run(
            kb.clone(),
            slug.clone(),
            storage.clone(),
            bus.clone(),
            quarantine,
            indexer_rx,
            None,
            None,
            crate::iframe::DEFAULT_HOST_SUFFIX.to_string(),
            crate::vcs::VersionsMode::Off,
            0,
            Vec::new(),
        ));

        let html_path = tmp.path().join("steady.html");
        std::fs::write(
            &html_path,
            "<html><title>Steady</title><body>x</body></html>",
        )
        .unwrap();

        // First reconcile: dedup MISS (no stored mtime yet) → emits modify;
        // the indexer indexes the file and stores its on-disk mtime.
        let mut rx = bus.subscribe();
        let (tx1, _rx1) = mpsc::channel(INGEST_QUEUE_CAPACITY);
        let _ = reconcile(
            IngestSink::new(tx1, bus.clone(), kb.clone()),
            storage.clone(),
            tmp.path().to_path_buf(),
            Vec::new(),
        )
        .await;
        let saw_indexed = timeout(Duration::from_secs(5), async {
            loop {
                match rx.recv().await {
                    Ok(env) if env.type_ == "artifact.indexed" => {
                        if env.payload["path"]
                            .as_str()
                            .unwrap_or("")
                            .ends_with("steady.html")
                        {
                            return true;
                        }
                    }
                    Ok(_) => continue,
                    Err(_) => return false,
                }
            }
        })
        .await
        .unwrap_or(false);
        assert!(saw_indexed, "initial reconcile should index the file");

        // Second reconcile, file byte- and mtime-unchanged: the stored mtime
        // (via list_reconcile_rows) must match disk and skip the re-emit.
        let mut rx2 = bus.subscribe();
        let (tx2, _rx2) = mpsc::channel(INGEST_QUEUE_CAPACITY);
        let summary = reconcile(
            IngestSink::new(tx2, bus.clone(), kb.clone()),
            storage.clone(),
            tmp.path().to_path_buf(),
            Vec::new(),
        )
        .await;
        assert_eq!(summary.files_walked, 1, "the file is still on disk");
        assert_eq!(summary.deletes_emitted, 0, "nothing vanished");

        // No watch.modify for the unchanged file within a short window —
        // proof the narrow projection carried mtime and the dedup fired.
        let saw_modify = timeout(Duration::from_millis(500), async {
            loop {
                match rx2.recv().await {
                    Ok(env) if env.type_ == "watch.modify" => {
                        if env.payload["path"]
                            .as_str()
                            .unwrap_or("")
                            .ends_with("steady.html")
                        {
                            return true;
                        }
                    }
                    Ok(_) => continue,
                    Err(_) => return false,
                }
            }
        })
        .await
        .unwrap_or(false);
        assert!(
            !saw_modify,
            "unchanged file must be deduped (no re-emit) on the second reconcile"
        );
    }

    // ---- X1 (v0.24) — configurable indexable-extension map ----

    /// Build an [`ExtensionMap`] from `(ext, pipeline)` pairs (test helper).
    fn ext_map(pairs: &[(&str, &str)]) -> crate::extmap::ExtensionMap {
        crate::extmap::ExtensionMap::from_config(
            &pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
        .unwrap()
    }

    /// X1 (test b) — a `txt = "markdown"` mapping indexes a `.txt` file
    /// end-to-end THROUGH the Markdown pipeline: the walk gate accepts `.txt`
    /// (sink-carried map) and `index_file` renders it via `extract_markdown`
    /// (indexer-carried map), so the `# H1` becomes the doc title — which the
    /// HTML pipeline would never produce for raw markdown text.
    #[tokio::test]
    async fn txt_mapped_to_markdown_indexes_via_markdown_pipeline() {
        let (bus, storage, kb, slug, tmp) = setup().await;
        let quarantine = tmp.path().join("quarantine");
        let map = ext_map(&[("txt", "markdown"), ("html", "html")]);

        // Drive `run_with_ingest` directly so the parse dispatch sees the
        // custom map; reconcile pushes through a sink carrying the SAME map.
        let (tx, rx) = mpsc::channel(INGEST_QUEUE_CAPACITY);
        let indexer = tokio::spawn(run_with_ingest(
            kb.clone(),
            slug.clone(),
            storage.clone(),
            bus.clone(),
            quarantine,
            rx,
            None,
            None,
            None,
            crate::iframe::DEFAULT_HOST_SUFFIX.to_string(),
            crate::vcs::VersionsMode::Off,
            Arc::new(crate::metrics::PipelineMetrics::disabled()),
            false,
            map.clone(),
            crate::exclusions::IngestGate::default(),
            empty_dedup_cache(),
        ));

        let txt_path = tmp.path().join("note.txt");
        std::fs::write(&txt_path, "# Hello Txt\n\nbody of the note\n").unwrap();

        let mut rx_evt = bus.subscribe();
        let sink =
            IngestSink::new(tx.clone(), bus.clone(), kb.clone()).with_extensions(map.clone());
        let summary = reconcile(sink, storage.clone(), tmp.path().to_path_buf(), Vec::new()).await;
        assert_eq!(
            summary.files_walked, 1,
            "the .txt file must pass the extension-map ingest gate"
        );

        let indexed = timeout(Duration::from_secs(5), async {
            loop {
                match rx_evt.recv().await {
                    Ok(env)
                        if env.type_ == "artifact.indexed"
                            && env.payload["path"]
                                .as_str()
                                .unwrap_or("")
                                .ends_with("note.txt") =>
                    {
                        return true
                    }
                    Ok(_) => continue,
                    Err(_) => return false,
                }
            }
        })
        .await
        .unwrap_or(false);
        assert!(indexed, ".txt mapped to markdown must index end-to-end");

        // Proof of the Markdown pipeline: title derived from the `# H1`.
        storage.ensure_fts_index().await.unwrap();
        let hits = storage.bm25_query("hello".into(), 5, false).await.unwrap();
        assert!(
            hits.iter().any(|h| h.title == "Hello Txt"),
            "markdown pipeline must derive the title from the `# H1`; got {:?}",
            hits.iter().map(|h| h.title.clone()).collect::<Vec<_>>()
        );

        drop(tx);
        let _ = timeout(Duration::from_secs(5), indexer).await;
    }

    /// X1 (test c) — THE TRAP: shrinking the map must DELETE now-unindexable
    /// stored docs even though the file still exists on disk. A `.txt` indexed
    /// under a wide map, then a reconcile under the default map (no `txt`),
    /// must route the de-mapped-but-existing file to the R2 Full cascade
    /// (its lance row is gone afterwards) — the `try_exists` guard alone would
    /// keep it forever.
    #[tokio::test]
    async fn map_shrink_deletes_now_unindexable_doc() {
        let (bus, storage, kb, slug, tmp) = setup().await;
        let quarantine = tmp.path().join("quarantine");
        let wide = ext_map(&[("txt", "markdown"), ("html", "html")]);

        // The running indexer's PARSE map can stay `wide` — the delete path
        // never consults it; the SHRINK is expressed by the reconcile sink's
        // map below.
        let (tx, rx) = mpsc::channel(INGEST_QUEUE_CAPACITY);
        let indexer = tokio::spawn(run_with_ingest(
            kb.clone(),
            slug.clone(),
            storage.clone(),
            bus.clone(),
            quarantine,
            rx,
            None,
            None,
            None,
            crate::iframe::DEFAULT_HOST_SUFFIX.to_string(),
            crate::vcs::VersionsMode::Off,
            Arc::new(crate::metrics::PipelineMetrics::disabled()),
            false,
            wide.clone(),
            crate::exclusions::IngestGate::default(),
            empty_dedup_cache(),
        ));

        let txt_path = tmp.path().join("ephemeral.txt");
        std::fs::write(&txt_path, "# Ephemeral\n\nbody\n").unwrap();

        // Index it under the wide map.
        let mut rx_idx = bus.subscribe();
        let sink_wide =
            IngestSink::new(tx.clone(), bus.clone(), kb.clone()).with_extensions(wide.clone());
        let _ = reconcile(
            sink_wide,
            storage.clone(),
            tmp.path().to_path_buf(),
            Vec::new(),
        )
        .await;
        let indexed = timeout(Duration::from_secs(5), async {
            loop {
                match rx_idx.recv().await {
                    Ok(env)
                        if env.type_ == "artifact.indexed"
                            && env.payload["path"]
                                .as_str()
                                .unwrap_or("")
                                .ends_with("ephemeral.txt") =>
                    {
                        return true
                    }
                    Ok(_) => continue,
                    Err(_) => return false,
                }
            }
        })
        .await
        .unwrap_or(false);
        assert!(indexed, "the .txt must index under the wide map first");
        assert_eq!(storage.count_rows().await.unwrap(), 1, "one row present");

        // SHRINK: reconcile with the DEFAULT map (no `txt`). The walk now skips
        // ephemeral.txt, so it's absent from `seen_paths`; the delete pass sees
        // it exists-but-de-mapped and emits a synthetic delete. The file is
        // still on disk, so it routes through the KeepUserData cascade — the
        // lance ROW is still dropped (asserted below), which is all this test
        // (no comments) observes; the sidecar-survival property has its own
        // test (`map_shrink_keeps_review_sidecar_for_present_file`).
        let mut rx_del = bus.subscribe();
        let sink_default = IngestSink::new(tx.clone(), bus.clone(), kb.clone()); // default map
        let summary = reconcile(
            sink_default,
            storage.clone(),
            tmp.path().to_path_buf(),
            Vec::new(),
        )
        .await;
        assert_eq!(
            summary.files_walked, 0,
            "the de-mapped .txt is no longer walked as indexable"
        );
        assert_eq!(
            summary.deletes_emitted, 1,
            "a map shrink must emit one synthetic delete for the de-mapped file"
        );

        let removed = timeout(Duration::from_secs(5), async {
            loop {
                match rx_del.recv().await {
                    Ok(env)
                        if env.type_ == "artifact.removed"
                            && env.payload["path"]
                                .as_str()
                                .unwrap_or("")
                                .ends_with("ephemeral.txt") =>
                    {
                        return true
                    }
                    Ok(_) => continue,
                    Err(_) => return false,
                }
            }
        })
        .await
        .unwrap_or(false);
        assert!(removed, "the de-mapped doc must be removed via the cascade");
        assert_eq!(
            storage.count_rows().await.unwrap(),
            0,
            "map shrink must delete the now-unindexable row (THE TRAP fix)"
        );
        // The file itself is untouched on disk — only the index row is gone.
        assert!(
            txt_path.exists(),
            "the shrink deletes the ROW, not the file"
        );

        drop(tx);
        let _ = timeout(Duration::from_secs(5), indexer).await;
    }

    /// X1 (test d) — a map SHRINK of a file that is STILL ON DISK must NOT
    /// destroy its `.review` comment sidecar. The de-mapped-but-present file
    /// routes through the KeepUserData cascade (not Full), so the lance row
    /// leaves the index (the TRAP fix stands) while the non-re-derivable
    /// comments survive — re-widening the map brings the doc, and its comments,
    /// back. Without this, an operator who narrows a per-kb map (e.g. adds
    /// `txt = "markdown"` not realising per-kb REPLACES) would silently and
    /// permanently lose every existing artifact's comments.
    #[tokio::test]
    async fn map_shrink_keeps_review_sidecar_for_present_file() {
        let (bus, storage, kb, slug, tmp) = setup().await;
        let quarantine = tmp.path().join("quarantine");
        let review_dir = tmp.path().join(".review");
        std::fs::create_dir_all(&review_dir).unwrap();
        let review_lock = Arc::new(tokio::sync::Mutex::new(()));
        let wide = ext_map(&[("txt", "markdown"), ("html", "html")]);

        // Indexer with the review_dir + review_lock wired so the cascade's fs
        // cleanup is ACTIVE (a Full cascade would reap the sidecar; KeepUserData
        // must not).
        let (tx, rx) = mpsc::channel(INGEST_QUEUE_CAPACITY);
        let indexer = tokio::spawn(run_with_ingest(
            kb.clone(),
            slug.clone(),
            storage.clone(),
            bus.clone(),
            quarantine,
            rx,
            None,
            Some(review_dir.clone()),
            Some(review_lock.clone()),
            crate::iframe::DEFAULT_HOST_SUFFIX.to_string(),
            crate::vcs::VersionsMode::Off,
            Arc::new(crate::metrics::PipelineMetrics::disabled()),
            false,
            wide.clone(),
            crate::exclusions::IngestGate::default(),
            empty_dedup_cache(),
        ));

        let txt_path = tmp.path().join("commented.txt");
        std::fs::write(&txt_path, "# Commented\n\nbody\n").unwrap();

        // Index under the wide map, capturing the artifact_id the pipeline
        // assigns (the SAME id `process_delete` will derive for the sidecar).
        let mut rx_idx = bus.subscribe();
        let sink_wide =
            IngestSink::new(tx.clone(), bus.clone(), kb.clone()).with_extensions(wide.clone());
        let _ = reconcile(
            sink_wide,
            storage.clone(),
            tmp.path().to_path_buf(),
            Vec::new(),
        )
        .await;
        let artifact_id = timeout(Duration::from_secs(5), async {
            loop {
                match rx_idx.recv().await {
                    Ok(env)
                        if env.type_ == "artifact.indexed"
                            && env.payload["path"]
                                .as_str()
                                .unwrap_or("")
                                .ends_with("commented.txt") =>
                    {
                        return env.payload["artifact_id"].as_str().map(String::from);
                    }
                    Ok(_) => continue,
                    Err(_) => return None,
                }
            }
        })
        .await
        .ok()
        .flatten()
        .expect("the .txt must index under the wide map first");
        assert_eq!(storage.count_rows().await.unwrap(), 1);

        // Drop a `.review/<id>.json` comment sidecar for the indexed doc.
        let sidecar = review_dir.join(format!("{artifact_id}.json"));
        std::fs::write(&sidecar, "{\"schema\":\"kb-comments/1\"}").unwrap();
        assert!(sidecar.exists());

        // SHRINK: reconcile under the DEFAULT map (no `txt`). commented.txt is
        // still on disk but de-mapped → KeepUserData cascade.
        let mut rx_del = bus.subscribe();
        let sink_default = IngestSink::new(tx.clone(), bus.clone(), kb.clone());
        let summary = reconcile(
            sink_default,
            storage.clone(),
            tmp.path().to_path_buf(),
            Vec::new(),
        )
        .await;
        assert_eq!(summary.deletes_emitted, 1);

        let removed = timeout(Duration::from_secs(5), async {
            loop {
                match rx_del.recv().await {
                    Ok(env)
                        if env.type_ == "artifact.removed"
                            && env.payload["path"]
                                .as_str()
                                .unwrap_or("")
                                .ends_with("commented.txt") =>
                    {
                        return true
                    }
                    Ok(_) => continue,
                    Err(_) => return false,
                }
            }
        })
        .await
        .unwrap_or(false);
        assert!(removed, "the de-mapped doc must leave the index");
        assert_eq!(
            storage.count_rows().await.unwrap(),
            0,
            "the index row still goes (TRAP fix intact)"
        );
        // THE FIX: the comment sidecar SURVIVES — the file is still on disk and
        // re-widening the map must restore its comments. A Full cascade here
        // would have reaped it.
        assert!(
            sidecar.exists(),
            "a map shrink of a still-present file must KEEP its .review sidecar"
        );
        assert!(txt_path.exists(), "the file itself is untouched");

        drop(tx);
        let _ = timeout(Duration::from_secs(5), indexer).await;
    }

    /// W0.8 — regression for a LIVE DATA-LOSS bug: a genuine content edit
    /// (an atomic temp-file+rename replace, or a burst of rapid saves) must
    /// NEVER destroy the `.review` comment sidecar. Drives the REAL watcher
    /// (`crate::watcher::Watcher`, not a synthetic `bus.emit`) so the test
    /// exercises whatever event shape `notify`/`notify-debouncer-full`
    /// actually synthesizes for these two edit patterns — both are confirmed
    /// prod repros (2026-07-15: 13 Edit-tool saves to
    /// corpus-a/dogfood-test-plan-2026-07.html deleted its sidecar,
    /// unlogged, with reconcile's deletes=0 proving the drop was event-
    /// driven, not the reconcile walk; 2026-07-17: burst edits + a
    /// `just ship` corpus rebuild — temp-file+rename atomic replace —
    /// dropped a sidecar with 7 resolved threads on research/read-first-ide.html).
    #[tokio::test]
    async fn atomic_replace_and_burst_edits_preserve_review_sidecar() {
        use crate::review::{Anchor, Author, Comment, CommentStatus, ReviewFile};
        use crate::watcher::{Watcher, WatcherConfig};
        use chrono::Utc;

        let (bus, storage, kb, slug, tmp) = setup().await;
        let review_dir = tmp.path().join(".review");
        std::fs::create_dir_all(&review_dir).unwrap();
        let review_lock = Arc::new(tokio::sync::Mutex::new(()));

        let (tx, rx) = mpsc::channel(INGEST_QUEUE_CAPACITY);
        let indexer = tokio::spawn(run_with_ingest(
            kb.clone(),
            slug.clone(),
            storage.clone(),
            bus.clone(),
            tmp.path().join("quarantine"),
            rx,
            None,
            Some(review_dir.clone()),
            Some(review_lock.clone()),
            crate::iframe::DEFAULT_HOST_SUFFIX.to_string(),
            crate::vcs::VersionsMode::Off,
            Arc::new(crate::metrics::PipelineMetrics::disabled()),
            false,
            crate::extmap::ExtensionMap::default(),
            crate::exclusions::IngestGate::default(),
            empty_dedup_cache(),
        ));

        let sink = IngestSink::new(tx.clone(), bus.clone(), kb.clone());
        let html_path = tmp.path().join("dogfood-test-plan-2026-07.html");
        std::fs::write(
            &html_path,
            "<html><body><section id=\"s1\"><h2>One</h2><p>v1</p></section></body></html>",
        )
        .unwrap();
        let id = crate::ids::ArtifactId::from_path("dogfood-test-plan-2026-07.html");

        // Real watcher, not a synthetic `bus.emit` — the whole point is to
        // exercise the actual fs-event shape a temp-file+rename replace and
        // a burst of rapid saves produce.
        let mut rx_bus = bus.subscribe();
        let _watcher = Watcher::start(
            WatcherConfig::new(kb.clone(), vec![tmp.path().to_path_buf()])
                .with_debounce(Duration::from_millis(80)),
            sink.clone(),
        )
        .unwrap();
        wait_for_indexed(&mut rx_bus, id.as_str()).await;

        // Seed a review file with 7 resolved threads (mirrors the 2026-07-17
        // incident) via the module API directly — no HTTP.
        let mut review = ReviewFile::empty_skeleton(&kb, id.as_str(), "dogfood");
        for i in 0..7 {
            review.comments.push(Comment {
                id: format!("c{i}"),
                status: CommentStatus::Resolved,
                file: id.as_str().to_string(),
                file_label: "main".into(),
                anchor: Anchor::Section {
                    id: "s1".into(),
                    tag: Some("section".into()),
                    snippet: None,
                },
                author: Author::You,
                body: format!("thread {i}"),
                created_at: Utc::now(),
                edited_at: None,
                replies: vec![],
                choices: vec![],
                attachments: vec![],
                user: None,
            });
        }
        let review_path = review_dir.join(format!("{}.json", id.as_str()));
        crate::review::save_atomic(&review_path, &review, None).unwrap();
        assert!(review_path.exists());

        // (a) burst-modify: several rapid in-place saves — the Edit-tool shape.
        for i in 0..8 {
            std::fs::write(
                &html_path,
                format!(
                    "<html><body><section id=\"s1\"><h2>One</h2><p>v{i}</p></section></body></html>"
                ),
            )
            .unwrap();
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        // (b) atomic temp-file + rename replace — the `just ship` build
        // pipeline shape (write to a sibling temp file, then rename it over
        // the target so readers never observe a torn write).
        let tmp_replace = tmp.path().join(".dogfood-test-plan-2026-07.html.tmp");
        std::fs::write(
            &tmp_replace,
            "<html><body><section id=\"s1\"><h2>One</h2><p>rebuilt</p></section></body></html>",
        )
        .unwrap();
        std::fs::rename(&tmp_replace, &html_path).unwrap();

        // Let the watcher/debouncer/indexer settle: wait for at least one
        // more artifact.indexed for this id after the rewrites, then give a
        // further margin for any trailing delete a coalesced event pair
        // might still queue behind it.
        let _ = timeout(Duration::from_secs(5), async {
            loop {
                match rx_bus.recv().await {
                    Ok(env)
                        if env.type_ == "artifact.indexed"
                            && env.payload["artifact_id"].as_str() == Some(id.as_str()) =>
                    {
                        return;
                    }
                    Ok(_) => continue,
                    Err(_) => return,
                }
            }
        })
        .await;
        tokio::time::sleep(Duration::from_millis(500)).await;

        assert!(
            review_path.exists(),
            "the review sidecar must survive a source rewrite via burst edits + an atomic \
             temp-file+rename replace (2026-07-15 / 2026-07-17 data-loss repro)"
        );
        let reloaded = crate::review::load(&review_path)
            .unwrap()
            .expect("review file must still parse");
        assert_eq!(
            reloaded.comments.len(),
            7,
            "all 7 resolved threads must survive intact"
        );
        assert!(
            reloaded
                .comments
                .iter()
                .all(|c| c.status == CommentStatus::Resolved),
            "thread resolution state must be unchanged"
        );

        drop(tx);
        let _ = timeout(SEC5, indexer).await;
    }

    /// W0.8 companion — a TRUE delete (the file is actually gone) must still
    /// run the Full cascade and reap the `.review` sidecar; the re-stat
    /// guard added for the bug above must not turn genuine deletes into
    /// silent no-ops (requirement: no behavior change for true deletions).
    #[tokio::test]
    async fn genuine_delete_still_removes_review_sidecar() {
        use crate::review::{Anchor, Author, Comment, CommentStatus, ReviewFile};
        use crate::watcher::{Watcher, WatcherConfig};
        use chrono::Utc;

        let (bus, storage, kb, slug, tmp) = setup().await;
        let review_dir = tmp.path().join(".review");
        std::fs::create_dir_all(&review_dir).unwrap();
        let review_lock = Arc::new(tokio::sync::Mutex::new(()));

        let (tx, rx) = mpsc::channel(INGEST_QUEUE_CAPACITY);
        let indexer = tokio::spawn(run_with_ingest(
            kb.clone(),
            slug.clone(),
            storage.clone(),
            bus.clone(),
            tmp.path().join("quarantine"),
            rx,
            None,
            Some(review_dir.clone()),
            Some(review_lock.clone()),
            crate::iframe::DEFAULT_HOST_SUFFIX.to_string(),
            crate::vcs::VersionsMode::Off,
            Arc::new(crate::metrics::PipelineMetrics::disabled()),
            false,
            crate::extmap::ExtensionMap::default(),
            crate::exclusions::IngestGate::default(),
            empty_dedup_cache(),
        ));

        let sink = IngestSink::new(tx.clone(), bus.clone(), kb.clone());
        let html_path = tmp.path().join("doomed-for-real.html");
        std::fs::write(&html_path, "<html><body>bye</body></html>").unwrap();
        let id = crate::ids::ArtifactId::from_path("doomed-for-real.html");

        let mut rx_bus = bus.subscribe();
        let _watcher = Watcher::start(
            WatcherConfig::new(kb.clone(), vec![tmp.path().to_path_buf()])
                .with_debounce(Duration::from_millis(80)),
            sink.clone(),
        )
        .unwrap();
        wait_for_indexed(&mut rx_bus, id.as_str()).await;

        let mut review = ReviewFile::empty_skeleton(&kb, id.as_str(), "doomed");
        review.comments.push(Comment {
            id: "c0".into(),
            status: CommentStatus::Open,
            file: id.as_str().to_string(),
            file_label: "main".into(),
            anchor: Anchor::Section {
                id: "s1".into(),
                tag: Some("section".into()),
                snippet: None,
            },
            author: Author::You,
            body: "still open".into(),
            created_at: Utc::now(),
            edited_at: None,
            replies: vec![],
            choices: vec![],
            attachments: vec![],
            user: None,
        });
        let review_path = review_dir.join(format!("{}.json", id.as_str()));
        crate::review::save_atomic(&review_path, &review, None).unwrap();
        assert!(review_path.exists());

        std::fs::remove_file(&html_path).unwrap();

        let removed = timeout(Duration::from_secs(5), async {
            loop {
                match rx_bus.recv().await {
                    Ok(env)
                        if env.type_ == "artifact.removed"
                            && env.payload["artifact_id"].as_str() == Some(id.as_str()) =>
                    {
                        return true;
                    }
                    Ok(_) => continue,
                    Err(_) => return false,
                }
            }
        })
        .await
        .unwrap_or(false);
        assert!(removed, "a genuine unlink must still fire artifact.removed");

        // Give the fs a beat in case of a scheduling race with the assertion.
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            !review_path.exists(),
            "a TRUE delete must still reap the review sidecar (no behavior regression)"
        );

        drop(tx);
        let _ = timeout(SEC5, indexer).await;
    }

    async fn setup() -> (
        Arc<EventBus>,
        StorageHandle,
        KbName,
        SourceSlug,
        tempfile::TempDir,
    ) {
        let tmp = tempdir().unwrap();
        let lance = tmp.path().join("lance");
        let sqlite = tmp.path().join("index.db");
        let storage = StorageActor::spawn(lance, sqlite, None).await.unwrap();
        let bus = Arc::new(EventBus::default());
        let kb = KbName::new("smoke").unwrap();
        let slug = SourceSlug::from_path(tmp.path());
        storage
            .upsert_source(slug.clone(), tmp.path().to_path_buf(), unix_now())
            .await
            .unwrap();
        (bus, storage, kb, slug, tmp)
    }

    /// X2 test helper — spawn `run_with_ingest` with the default map and the
    /// given SHARED gate (the same one the test's sink carries).
    fn spawn_gated_indexer(
        bus: &Arc<EventBus>,
        storage: &StorageHandle,
        kb: &KbName,
        slug: &SourceSlug,
        tmp: &tempfile::TempDir,
        gate: crate::exclusions::IngestGate,
    ) -> (mpsc::Sender<WatchWork>, tokio::task::JoinHandle<()>) {
        let (tx, rx) = mpsc::channel(INGEST_QUEUE_CAPACITY);
        let handle = tokio::spawn(run_with_ingest(
            kb.clone(),
            slug.clone(),
            storage.clone(),
            bus.clone(),
            tmp.path().join("quarantine"),
            rx,
            None,
            None,
            None,
            crate::iframe::DEFAULT_HOST_SUFFIX.to_string(),
            crate::vcs::VersionsMode::Off,
            Arc::new(crate::metrics::PipelineMetrics::disabled()),
            false,
            crate::extmap::ExtensionMap::default(),
            gate,
            empty_dedup_cache(),
        ));
        (tx, handle)
    }

    /// X2 test — generous positive-assertion timeout.
    const SEC5: Duration = Duration::from_secs(5);

    /// X2 test helper — await a bus event of `type_` whose payload `path`
    /// ends with `suffix`, within `wait`. `false` on timeout.
    async fn wait_event_for_path(
        rx: &mut tokio::sync::broadcast::Receiver<crate::types::Envelope>,
        type_: &str,
        suffix: &str,
        wait: Duration,
    ) -> bool {
        timeout(wait, async {
            loop {
                match rx.recv().await {
                    Ok(env)
                        if env.type_ == type_
                            && env.payload["path"].as_str().unwrap_or("").ends_with(suffix) =>
                    {
                        return true
                    }
                    Ok(_) => continue,
                    Err(_) => return false,
                }
            }
        })
        .await
        .unwrap_or(false)
    }

    /// X2 GOLDEN — the per-file exclusion lifecycle, end to end:
    /// exclude → the doc leaves the index (row count = gallery counts),
    /// search, and the dedup cache; a subsequent ingest event for it is
    /// IGNORED (the `prepare_doc` gate — the per-item backstop); un-exclude →
    /// the forced nudge reindexes it and search finds it again.
    #[tokio::test]
    async fn exclusion_lifecycle_excludes_ignores_events_and_reincludes() {
        let (bus, storage, kb, slug, tmp) = setup().await;
        let gate = crate::exclusions::IngestGate::default();
        let (tx, indexer) = spawn_gated_indexer(&bus, &storage, &kb, &slug, &tmp, gate.clone());
        let sink = IngestSink::new(tx.clone(), bus.clone(), kb.clone()).with_gate(gate.clone());

        let keep = tmp.path().join("keep.html");
        let excludee = tmp.path().join("drop.html");
        std::fs::write(
            &keep,
            "<html><title>Keep</title><body>evergreen body</body></html>",
        )
        .unwrap();
        std::fs::write(
            &excludee,
            "<html><title>Drop</title><body>ephemeralword body</body></html>",
        )
        .unwrap();

        let mut rx_idx = bus.subscribe();
        sink.send(WatchKind::Created, keep.clone(), false).await;
        sink.send(WatchKind::Created, excludee.clone(), false).await;
        assert!(
            wait_event_for_path(&mut rx_idx, "artifact.indexed", "keep.html", SEC5).await,
            "keep.html must index"
        );
        assert!(
            wait_event_for_path(&mut rx_idx, "artifact.indexed", "drop.html", SEC5).await,
            "drop.html must index"
        );
        assert_eq!(storage.count_rows().await.unwrap(), 2);
        storage.ensure_fts_index().await.unwrap();
        let hits = storage
            .bm25_query("ephemeralword".into(), 5, false)
            .await
            .unwrap();
        assert!(!hits.is_empty(), "excludee searchable before exclusion");

        // EXCLUDE — sqlite row + gate entry + the exclusion-shaped delete
        // through the ingest channel (KeepUserData cascade, artifact.removed).
        let mut rx_rm = bus.subscribe();
        let added = crate::exclusions::exclude_file(
            &storage,
            &sink,
            tmp.path(),
            "drop.html",
            Some("too noisy".into()),
        )
        .await
        .unwrap();
        assert!(added, "first exclusion reports added");
        assert!(
            wait_event_for_path(&mut rx_rm, "artifact.removed", "drop.html", SEC5).await,
            "exclusion must emit artifact.removed"
        );
        assert_eq!(
            storage.count_rows().await.unwrap(),
            1,
            "excluded doc leaves the index (gallery counts drop)"
        );
        let hits = storage
            .bm25_query("ephemeralword".into(), 5, false)
            .await
            .unwrap();
        assert!(hits.is_empty(), "excluded doc must vanish from search");
        let rows = storage.list_exclusions().await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path, "drop.html");
        assert_eq!(rows[0].note.as_deref(), Some("too noisy"));

        // An ingest event for the excluded file is IGNORED: it flows to
        // `prepare_doc`, whose top gate returns the Ok(None) skip — no
        // artifact.indexed, no row.
        let mut rx_ig = bus.subscribe();
        sink.send(WatchKind::Modified, excludee.clone(), false)
            .await;
        assert!(
            !wait_event_for_path(
                &mut rx_ig,
                "artifact.indexed",
                "drop.html",
                Duration::from_millis(800)
            )
            .await,
            "an event for an excluded file must not reindex it"
        );
        assert_eq!(storage.count_rows().await.unwrap(), 1, "still excluded");

        // UN-EXCLUDE — row + gate entry go; the forced nudge reindexes.
        let mut rx_back = bus.subscribe();
        let removed = crate::exclusions::include_file(&storage, &sink, tmp.path(), "drop.html")
            .await
            .unwrap();
        assert!(removed, "include reports removal");
        assert!(
            wait_event_for_path(&mut rx_back, "artifact.indexed", "drop.html", SEC5).await,
            "re-include must trigger an immediate reindex"
        );
        assert_eq!(storage.count_rows().await.unwrap(), 2, "doc is back");
        assert!(storage.list_exclusions().await.unwrap().is_empty());
        let hits = storage
            .bm25_query("ephemeralword".into(), 5, false)
            .await
            .unwrap();
        assert!(!hits.is_empty(), "re-included doc searchable again");

        drop(sink);
        drop(tx);
        let _ = timeout(SEC5, indexer).await;
    }

    /// X2/D6 GOLDEN — a paused source ingests NOTHING until unpaused
    /// (pre-v0.24 `sources.paused` was stored but silently ignored). The
    /// `prepare_doc` top gate is the seam exercised here (an event already in
    /// flight / pushed directly); the walk + watcher seams have their own
    /// tests.
    #[tokio::test]
    async fn paused_source_blocks_ingest_until_unpaused() {
        let (bus, storage, kb, slug, tmp) = setup().await;
        let gate = crate::exclusions::IngestGate::default();
        let (tx, indexer) = spawn_gated_indexer(&bus, &storage, &kb, &slug, &tmp, gate.clone());
        let sink = IngestSink::new(tx.clone(), bus.clone(), kb.clone()).with_gate(gate.clone());

        let p = tmp.path().join("waiting.html");
        std::fs::write(
            &p,
            "<html><title>Waiting</title><body>patient body</body></html>",
        )
        .unwrap();

        gate.set_paused(true);
        let mut rx_evt = bus.subscribe();
        sink.send(WatchKind::Created, p.clone(), false).await;
        assert!(
            !wait_event_for_path(
                &mut rx_evt,
                "artifact.indexed",
                "waiting.html",
                Duration::from_millis(800)
            )
            .await,
            "a paused source must not index"
        );
        assert_eq!(
            storage.count_rows().await.unwrap(),
            0,
            "no ingest while paused"
        );

        // Unpause + re-nudge (production: the resume route flips the gate and
        // the next reconcile tick re-walks the source).
        gate.set_paused(false);
        let mut rx_ok = bus.subscribe();
        sink.send(WatchKind::Created, p.clone(), false).await;
        assert!(
            wait_event_for_path(&mut rx_ok, "artifact.indexed", "waiting.html", SEC5).await,
            "after resume the file must index"
        );
        assert_eq!(storage.count_rows().await.unwrap(), 1);

        drop(sink);
        drop(tx);
        let _ = timeout(SEC5, indexer).await;
    }

    /// X2 — THE TRAP, self-heal arm: an exclusion recorded while the daemon
    /// was down (sqlite row exists, no live cascade ran) leaves an excluded-
    /// but-still-indexed row whose file is still on disk. Reconcile must
    /// produce the EXPLICIT delete decision (the walk's absence alone deletes
    /// nothing — `reconcile` keeps every present-and-mapped file), routing it
    /// through the KeepUserData cascade; and the orphan sweep must EXEMPT the
    /// excluded doc's kept history rows on subsequent passes.
    #[tokio::test]
    async fn reconcile_deletes_excluded_but_indexed_row_and_spares_kept_history() {
        let (bus, storage, kb, slug, tmp) = setup().await;
        let gate = crate::exclusions::IngestGate::default();
        let (tx, indexer) = spawn_gated_indexer(&bus, &storage, &kb, &slug, &tmp, gate.clone());
        let sink = IngestSink::new(tx.clone(), bus.clone(), kb.clone()).with_gate(gate.clone());

        let p = tmp.path().join("legacy.html");
        std::fs::write(
            &p,
            "<html><title>Legacy</title><body>old body</body></html>",
        )
        .unwrap();
        let mut rx_idx = bus.subscribe();
        sink.send(WatchKind::Created, p.clone(), false).await;
        assert!(
            wait_event_for_path(&mut rx_idx, "artifact.indexed", "legacy.html", SEC5).await,
            "seed index"
        );

        // Reading history for it — the user data the KeepUserData cascade
        // keeps and the sweep must not reap.
        let id = ArtifactId::from_path("legacy.html");
        storage
            .history_record_open(id.as_str().to_string(), unix_now(), None, "operator".into())
            .await
            .unwrap();

        // Simulate "excluded while the daemon was down": durable row + gate
        // entry, NO live cascade.
        storage
            .add_exclusion("legacy.html".into(), unix_now(), None)
            .await
            .unwrap();
        gate.add_excluded("legacy.html");

        let mut rx_rm = bus.subscribe();
        let summary = reconcile(
            sink.clone(),
            storage.clone(),
            tmp.path().to_path_buf(),
            Vec::new(),
        )
        .await;
        assert_eq!(
            summary.files_walked, 0,
            "the excluded file must be invisible to the walk"
        );
        assert_eq!(
            summary.deletes_emitted, 1,
            "reconcile must produce the explicit exclusion delete decision"
        );
        assert!(
            wait_event_for_path(&mut rx_rm, "artifact.removed", "legacy.html", SEC5).await,
            "the KeepUserData cascade must remove the row"
        );
        assert_eq!(storage.count_rows().await.unwrap(), 0, "row gone");
        assert!(
            p.exists(),
            "the FILE is untouched — exclusion deletes the row"
        );

        // Second pass: idempotent (row already gone) AND the orphan sweep
        // exempts the excluded doc's kept history (the gate snapshot feeds
        // `sweep_orphans`; without it the history row is a sweep candidate
        // since its id no longer exists in lance).
        let s2 = reconcile(
            sink.clone(),
            storage.clone(),
            tmp.path().to_path_buf(),
            Vec::new(),
        )
        .await;
        assert_eq!(s2.deletes_emitted, 0, "no repeat deletes for a gone row");
        let opens = storage
            .history_opens_in_window(0, i64::MAX, 100)
            .await
            .unwrap();
        assert!(
            opens
                .iter()
                .any(|h| h.artifact_id.as_deref() == Some(id.as_str())),
            "the excluded doc's kept history must survive the orphan sweep"
        );

        drop(sink);
        drop(tx);
        let _ = timeout(SEC5, indexer).await;
    }

    #[tokio::test]
    async fn create_event_indexes_file_and_emits_artifact_indexed() {
        let (bus, storage, kb, slug, tmp) = setup().await;
        let quarantine = tmp.path().join("quarantine");

        // Spawn the indexer.
        let indexer_rx = bus.subscribe();
        let bus_for_indexer = bus.clone();
        let storage_for_indexer = storage.clone();
        let kb_for_indexer = kb.clone();
        let slug_for_indexer = slug.clone();
        let quarantine_for_indexer = quarantine.clone();
        let _indexer = tokio::spawn(async move {
            run(
                kb_for_indexer,
                slug_for_indexer,
                storage_for_indexer,
                bus_for_indexer,
                quarantine_for_indexer,
                indexer_rx,
                None,
                None,
                crate::iframe::DEFAULT_HOST_SUFFIX.to_string(),
                crate::vcs::VersionsMode::Off,
                0,
                Vec::new(),
            )
            .await
        });

        // Subscribe BEFORE emitting.
        let mut rx = bus.subscribe();

        // Simulate a watch.create event.
        let html_path = tmp.path().join("a.html");
        std::fs::write(
            &html_path,
            "<html><title>Hello</title><body>world</body></html>",
        )
        .unwrap();
        bus.emit(
            "watch.create",
            json!({
                "kb": kb.as_str(),
                "path": html_path.to_string_lossy(),
            }),
        );

        // Wait for artifact.indexed.
        let env = timeout(Duration::from_secs(5), async {
            loop {
                match rx.recv().await {
                    Ok(env) if env.type_ == "artifact.indexed" => return env,
                    _ => continue,
                }
            }
        })
        .await
        .expect("timeout waiting for artifact.indexed");

        assert_eq!(env.payload["kb"].as_str().unwrap(), "smoke");
        assert_eq!(env.payload["change_kind"].as_str().unwrap(), "created");

        // BM25 query should find it.
        storage.ensure_fts_index().await.unwrap();
        let hits = storage.bm25_query("hello".into(), 5, false).await.unwrap();
        assert!(hits.iter().any(|h| h.title == "Hello"));
    }

    #[tokio::test]
    async fn successful_index_auto_dismisses_open_errors_for_path() {
        // A path that failed earlier (environmental error, content
        // unchanged) should have its open error rows dismissed when a
        // later index attempt succeeds. Pre-fix, only DIFFERENT-hash
        // rows were cleared, so a same-hash transient failure (e.g. a
        // lance OOM that fixed itself after a config bump) left stale
        // rows in the Errors tab indefinitely.
        let (bus, storage, kb, slug, tmp) = setup().await;
        let quarantine = tmp.path().join("quarantine");
        let html_path = tmp.path().join("flaky.html");
        std::fs::write(
            &html_path,
            "<html><title>Flaky</title><body>retry me</body></html>",
        )
        .unwrap();

        // Pre-seed two open errors for this path: one with the same
        // content_hash we expect index_file to compute, one with None.
        // The kb_core::ids hash isn't directly exposed; the indexer
        // re-derives it anyway. We just need ANY hash here — the new
        // path-only clear doesn't filter by hash.
        storage
            .record_error(
                "storage".into(),
                slug.clone(),
                html_path.clone(),
                "synthetic pre-existing failure".into(),
                Some("stale-hash-aa".into()),
                unix_now(),
            )
            .await
            .unwrap();
        storage
            .record_error(
                "storage".into(),
                slug.clone(),
                html_path.clone(),
                "synthetic hashless failure".into(),
                None,
                unix_now() + 1,
            )
            .await
            .unwrap();
        assert_eq!(
            storage.list_open_errors().await.unwrap().len(),
            2,
            "two open errors before successful index"
        );

        let indexer_rx = bus.subscribe();
        let bus_for_indexer = bus.clone();
        let storage_for_indexer = storage.clone();
        let kb_for_indexer = kb.clone();
        let slug_for_indexer = slug.clone();
        let quarantine_for_indexer = quarantine.clone();
        let _indexer = tokio::spawn(async move {
            run(
                kb_for_indexer,
                slug_for_indexer,
                storage_for_indexer,
                bus_for_indexer,
                quarantine_for_indexer,
                indexer_rx,
                None,
                None,
                crate::iframe::DEFAULT_HOST_SUFFIX.to_string(),
                crate::vcs::VersionsMode::Off,
                0,
                Vec::new(),
            )
            .await
        });

        let mut rx = bus.subscribe();
        bus.emit(
            "watch.create",
            json!({
                "kb": kb.as_str(),
                "path": html_path.to_string_lossy(),
            }),
        );

        // Collect events until we've seen both artifact.indexed AND
        // error.dismissed (order isn't guaranteed across mpsc fan-out).
        let mut saw_indexed = false;
        let mut saw_dismissed = false;
        let mut dismissed_count = 0u64;
        let mut dismissed_path: Option<String> = None;
        timeout(Duration::from_secs(5), async {
            while !(saw_indexed && saw_dismissed) {
                let env = rx.recv().await.expect("bus closed");
                match env.type_.as_str() {
                    "artifact.indexed" => saw_indexed = true,
                    "error.dismissed" => {
                        saw_dismissed = true;
                        dismissed_count = env.payload["count"].as_u64().unwrap_or(0);
                        dismissed_path = env.payload["path"].as_str().map(str::to_string);
                    }
                    _ => {}
                }
            }
        })
        .await
        .expect("timeout waiting for indexed + dismissed");

        assert_eq!(dismissed_count, 2, "both pre-seeded errors dismissed");
        assert_eq!(
            dismissed_path.as_deref(),
            Some(html_path.to_string_lossy().as_ref()),
        );
        let open = storage.list_open_errors().await.unwrap();
        assert!(
            open.is_empty(),
            "no open errors remain after successful index, got {open:?}"
        );
    }

    #[tokio::test]
    async fn delete_event_removes_row() {
        let (bus, storage, kb, slug, tmp) = setup().await;
        let quarantine = tmp.path().join("quarantine");

        let indexer_rx = bus.subscribe();
        let bus_for_indexer = bus.clone();
        let storage_for_indexer = storage.clone();
        let kb_for_indexer = kb.clone();
        let slug_for_indexer = slug.clone();
        let quarantine_for_indexer = quarantine.clone();
        let _indexer = tokio::spawn(async move {
            run(
                kb_for_indexer,
                slug_for_indexer,
                storage_for_indexer,
                bus_for_indexer,
                quarantine_for_indexer,
                indexer_rx,
                None,
                None,
                crate::iframe::DEFAULT_HOST_SUFFIX.to_string(),
                crate::vcs::VersionsMode::Off,
                0,
                Vec::new(),
            )
            .await
        });

        // Index one file, then delete.
        let html_path = tmp.path().join("doomed.html");
        std::fs::write(&html_path, "<html><title>Doomed</title></html>").unwrap();

        let mut rx = bus.subscribe();
        bus.emit(
            "watch.create",
            json!({"kb": kb.as_str(), "path": html_path.to_string_lossy()}),
        );

        // Wait for artifact.indexed.
        timeout(Duration::from_secs(5), async {
            loop {
                match rx.recv().await {
                    Ok(env) if env.type_ == "artifact.indexed" => return env,
                    _ => continue,
                }
            }
        })
        .await
        .expect("indexed timeout");
        assert_eq!(storage.count_rows().await.unwrap(), 1);

        // W0.8 — `process_delete` now re-stats the path right before running
        // the Full cascade (see the guard's comment), so a synthetic
        // `watch.delete` for a file that's still ON DISK is correctly treated
        // as a transient/spurious signal and skipped. Actually remove the
        // file so this test still exercises a REAL delete.
        std::fs::remove_file(&html_path).unwrap();

        // Now emit delete.
        bus.emit(
            "watch.delete",
            json!({"kb": kb.as_str(), "path": html_path.to_string_lossy()}),
        );

        timeout(Duration::from_secs(5), async {
            loop {
                match rx.recv().await {
                    Ok(env) if env.type_ == "artifact.removed" => return env,
                    _ => continue,
                }
            }
        })
        .await
        .expect("removed timeout");
        assert_eq!(storage.count_rows().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn modify_event_replaces_row_in_place() {
        let (bus, storage, kb, slug, tmp) = setup().await;
        let quarantine = tmp.path().join("quarantine");

        let indexer_rx = bus.subscribe();
        let bus_for_indexer = bus.clone();
        let storage_for_indexer = storage.clone();
        let kb_for_indexer = kb.clone();
        let slug_for_indexer = slug.clone();
        let quarantine_for_indexer = quarantine.clone();
        let _indexer = tokio::spawn(async move {
            run(
                kb_for_indexer,
                slug_for_indexer,
                storage_for_indexer,
                bus_for_indexer,
                quarantine_for_indexer,
                indexer_rx,
                None,
                None,
                crate::iframe::DEFAULT_HOST_SUFFIX.to_string(),
                crate::vcs::VersionsMode::Off,
                0,
                Vec::new(),
            )
            .await
        });

        let html_path = tmp.path().join("v.html");
        std::fs::write(&html_path, "<html><title>v1</title></html>").unwrap();
        let mut rx = bus.subscribe();

        bus.emit(
            "watch.create",
            json!({"kb": kb.as_str(), "path": html_path.to_string_lossy()}),
        );

        timeout(Duration::from_secs(5), async {
            loop {
                match rx.recv().await {
                    Ok(env) if env.type_ == "artifact.indexed" => return env,
                    _ => continue,
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(storage.count_rows().await.unwrap(), 1);

        std::fs::write(&html_path, "<html><title>v2</title></html>").unwrap();
        bus.emit(
            "watch.modify",
            json!({"kb": kb.as_str(), "path": html_path.to_string_lossy()}),
        );

        timeout(Duration::from_secs(5), async {
            loop {
                match rx.recv().await {
                    Ok(env) if env.type_ == "artifact.indexed" => {
                        if env.payload["change_kind"].as_str() == Some("modified") {
                            return env;
                        }
                    }
                    _ => continue,
                }
            }
        })
        .await
        .unwrap();
        // Row replaced, not appended.
        assert_eq!(storage.count_rows().await.unwrap(), 1);
    }

    /// Dedup PRE-gate (`process_one` in this file): a `watch.modify` /
    /// reconcile re-emit for a file whose bytes are byte-identical to the
    /// last successfully-indexed version MUST be a complete no-op — no
    /// `artifact.indexed`, and (the v0.16 fix) no sqlite run row and no
    /// `index.start` / `index.file` / `index.complete` lifecycle triple
    /// either. The safety-net reconciler re-emits a `watch.modify` for
    /// EVERY file every `reconcile_secs`; before the pre-gate each
    /// unchanged file still spun up a run + a lifecycle triple, flooding
    /// SSE subscribers and overrunning the 1024-slot broadcast channel
    /// (the "indexer lagged: dropped envelopes" storm). The SPA detail
    /// view also remounts its cross-origin iframe on every event, losing
    /// the reader's scroll position — see `web/src/routes/detail.tsx`.
    ///
    /// A trailing create of a DISTINCT file is the in-order barrier: the
    /// indexer is a single consumer, so once the sentinel's
    /// `index.complete` lands the byte-identical modify ahead of it has
    /// already been fully processed — and must have produced nothing.
    #[tokio::test]
    async fn byte_identical_reindex_emits_no_run_or_lifecycle_events() {
        let (bus, storage, kb, slug, tmp) = setup().await;
        let quarantine = tmp.path().join("quarantine");

        let indexer_rx = bus.subscribe();
        let bus_for_indexer = bus.clone();
        let storage_for_indexer = storage.clone();
        let kb_for_indexer = kb.clone();
        let slug_for_indexer = slug.clone();
        let quarantine_for_indexer = quarantine.clone();
        let _indexer = tokio::spawn(async move {
            run(
                kb_for_indexer,
                slug_for_indexer,
                storage_for_indexer,
                bus_for_indexer,
                quarantine_for_indexer,
                indexer_rx,
                None,
                None,
                crate::iframe::DEFAULT_HOST_SUFFIX.to_string(),
                crate::vcs::VersionsMode::Off,
                0,
                Vec::new(),
            )
            .await
        });

        let html_path = tmp.path().join("static.html");
        let html = "<html><title>Static</title><body>same bytes forever</body></html>";
        std::fs::write(&html_path, html).unwrap();
        let mut rx = bus.subscribe();

        // 1. Create — drain its FULL lifecycle so the window below starts
        //    clean. `index.complete` is the last event process_one emits.
        bus.emit(
            "watch.create",
            json!({"kb": kb.as_str(), "path": html_path.to_string_lossy()}),
        );
        let saw_create_indexed = timeout(Duration::from_secs(5), async {
            let mut indexed = false;
            loop {
                match rx.recv().await {
                    Ok(env) if env.type_ == "artifact.indexed" => indexed = true,
                    Ok(env) if env.type_ == "index.complete" => return indexed,
                    _ => continue,
                }
            }
        })
        .await
        .expect("create lifecycle timeout");
        assert!(saw_create_indexed, "create must index static.html");

        // 2. Re-write byte-identical content (simulates `touch` / a
        //    reconcile re-emit / editor save with no diff) — must be a
        //    total no-op.
        std::fs::write(&html_path, html).unwrap();
        bus.emit(
            "watch.modify",
            json!({"kb": kb.as_str(), "path": html_path.to_string_lossy()}),
        );

        // 3. Trailing sentinel — a DISTINCT file that really indexes,
        //    giving an in-order barrier for the no-op modify ahead of it.
        let sentinel = tmp.path().join("sentinel.html");
        std::fs::write(
            &sentinel,
            "<html><title>Sentinel</title><body>distinct bytes</body></html>",
        )
        .unwrap();
        bus.emit(
            "watch.create",
            json!({"kb": kb.as_str(), "path": sentinel.to_string_lossy()}),
        );

        // 4. Collect every event until the sentinel's `index.complete`
        //    (correlated by run id). The no-op modify must contribute
        //    nothing to the window.
        let window = timeout(Duration::from_secs(5), async {
            let mut events: Vec<(String, String)> = Vec::new();
            let mut sentinel_run: Option<String> = None;
            loop {
                let env = match rx.recv().await {
                    Ok(e) => e,
                    Err(_) => break,
                };
                let run = env
                    .payload
                    .get("run")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let path = env
                    .payload
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if env.type_ == "index.file" && path.ends_with("sentinel.html") {
                    sentinel_run = run.clone();
                }
                let barrier =
                    env.type_ == "index.complete" && sentinel_run.is_some() && run == sentinel_run;
                events.push((env.type_.clone(), path));
                if barrier {
                    break;
                }
            }
            events
        })
        .await
        .expect("timed out waiting for sentinel index.complete");

        // Inspect only indexer-PRODUCED events. The `watch.*` entries in
        // the window are our own emitted stimuli echoed back over the same
        // bus, not indexer output — they're irrelevant to the contract.
        let produced: Vec<&(String, String)> = window
            .iter()
            .filter(|(t, _)| t.starts_with("index.") || t == "artifact.indexed")
            .collect();
        // No produced event may reference the unchanged file.
        assert!(
            !produced.iter().any(|(_, p)| p.ends_with("static.html")),
            "byte-identical modify must produce no events for static.html; window: {window:?}",
        );
        // Exactly one run's worth of lifecycle events — the sentinel's.
        // The no-op modify must add neither a start nor a complete.
        assert_eq!(
            produced.iter().filter(|(t, _)| t == "index.start").count(),
            1,
            "dedup pre-gate must suppress index.start on the no-op pass; window: {window:?}",
        );
        assert_eq!(
            produced
                .iter()
                .filter(|(t, _)| t == "index.complete")
                .count(),
            1,
            "dedup pre-gate must suppress index.complete on the no-op pass; window: {window:?}",
        );
        // Two rows: static + sentinel, each indexed exactly once.
        assert_eq!(storage.count_rows().await.unwrap(), 2);
    }

    /// `force = true` (operator reindex) must bypass BOTH the pre-gate and
    /// the in-band gate: re-running an unchanged artifact is the recovery
    /// path for cross-artifact link resolution, so it must still produce a
    /// fresh `artifact.indexed` even though the bytes never changed.
    #[tokio::test]
    async fn forced_reindex_of_unchanged_bytes_still_runs() {
        let (bus, storage, kb, slug, tmp) = setup().await;
        let quarantine = tmp.path().join("quarantine");

        let indexer_rx = bus.subscribe();
        let bus_for_indexer = bus.clone();
        let storage_for_indexer = storage.clone();
        let kb_for_indexer = kb.clone();
        let slug_for_indexer = slug.clone();
        let quarantine_for_indexer = quarantine.clone();
        let _indexer = tokio::spawn(async move {
            run(
                kb_for_indexer,
                slug_for_indexer,
                storage_for_indexer,
                bus_for_indexer,
                quarantine_for_indexer,
                indexer_rx,
                None,
                None,
                crate::iframe::DEFAULT_HOST_SUFFIX.to_string(),
                crate::vcs::VersionsMode::Off,
                0,
                Vec::new(),
            )
            .await
        });

        let html_path = tmp.path().join("doc.html");
        let html = "<html><title>Doc</title><body>frozen bytes</body></html>";
        std::fs::write(&html_path, html).unwrap();
        let mut rx = bus.subscribe();

        // Create + drain its lifecycle.
        bus.emit(
            "watch.create",
            json!({"kb": kb.as_str(), "path": html_path.to_string_lossy()}),
        );
        timeout(Duration::from_secs(5), async {
            loop {
                match rx.recv().await {
                    Ok(env) if env.type_ == "index.complete" => return,
                    _ => continue,
                }
            }
        })
        .await
        .expect("create lifecycle timeout");

        // Forced modify of byte-identical content — must re-emit
        // artifact.indexed despite the unchanged bytes. (If the pre-gate
        // wrongly swallowed force, we'd hit index.complete first.)
        bus.emit(
            "watch.modify",
            json!({"kb": kb.as_str(), "path": html_path.to_string_lossy(), "force": true}),
        );
        let reindexed = timeout(Duration::from_secs(5), async {
            loop {
                match rx.recv().await {
                    Ok(env) if env.type_ == "artifact.indexed" => return true,
                    Ok(env) if env.type_ == "index.complete" => return false,
                    _ => continue,
                }
            }
        })
        .await
        .expect("forced reindex timeout");
        assert!(
            reindexed,
            "force=true must re-emit artifact.indexed even for unchanged bytes",
        );
    }

    /// G7 — the decouple's core guarantee: a bulk push LARGER than the ingest
    /// channel bound is fully indexed with ZERO loss, because the bounded mpsc
    /// back-pressures the producer (`send().await` blocks when full) instead of
    /// dropping like the old broadcast bus. Drives `run_with_ingest` directly.
    // invariant:17 backpressure-mpsc
    #[tokio::test]
    async fn run_with_ingest_indexes_every_item_under_backpressure() {
        let (bus, storage, kb, slug, tmp) = setup().await;
        let quarantine = tmp.path().join("quarantine");

        // Deliberately tiny channel: a bulk burst of N >> 4 would overflow a
        // same-sized broadcast, but here it just back-pressures.
        let (tx, rx) = mpsc::channel::<WatchWork>(4);
        let bus_for_indexer = bus.clone();
        let storage_for_indexer = storage.clone();
        let kb_for_indexer = kb.clone();
        let slug_for_indexer = slug.clone();
        let quarantine_for_indexer = quarantine.clone();
        let indexer = tokio::spawn(async move {
            run_with_ingest(
                kb_for_indexer,
                slug_for_indexer,
                storage_for_indexer,
                bus_for_indexer,
                quarantine_for_indexer,
                rx,
                None,
                None,
                None,
                crate::iframe::DEFAULT_HOST_SUFFIX.to_string(),
                crate::vcs::VersionsMode::Off,
                Arc::new(crate::metrics::PipelineMetrics::disabled()),
                false,
                crate::extmap::ExtensionMap::default(),
                crate::exclusions::IngestGate::default(),
                empty_dedup_cache(),
            )
            .await
        });

        const N: usize = 25;
        for i in 0..N {
            let p = tmp.path().join(format!("doc{i}.html"));
            std::fs::write(
                &p,
                format!("<html><title>Doc {i}</title><body>body {i}</body></html>"),
            )
            .unwrap();
            // Back-pressures once 4 are queued — the send awaits, never drops.
            tx.send(WatchWork {
                kind: WatchKind::Created,
                path: p,
                force: false,
                keep_user_data: false,
            })
            .await
            .unwrap();
        }
        drop(tx); // close the channel → run_with_ingest drains then returns

        // The indexer exits once every sender drops AND the queue is drained.
        timeout(Duration::from_secs(30), indexer)
            .await
            .expect("indexer did not drain + exit")
            .expect("indexer task panicked");

        assert_eq!(
            storage.count_rows().await.unwrap() as usize,
            N,
            "every pushed item must be indexed — no loss under back-pressure",
        );
    }

    /// GC-B7 — the scale-test regression this whole change targets: drive
    /// ~100 small docs through the REAL ingest pipeline via a channel large
    /// enough that every item is already queued before the indexer's first
    /// `rx.recv()`, so `drain_ingest_batch` always has a full backlog to
    /// opportunistically batch from (deterministic — no timing race).
    /// Asserts (a) every doc lands + one `artifact.indexed` fires per doc,
    /// (b) the Lance dataset's manifest-version count is FAR below the doc
    /// count (an upper bound that fails outright on the old one-commit-
    /// per-doc pattern), and (c) an explicit compact+reclaim pass (a
    /// zero-minute retention override — production's real window wouldn't
    /// see anything eligible yet in a fast test) shrinks it further while
    /// every row survives intact.
    #[tokio::test]
    async fn ingest_batching_shares_one_lance_commit_and_reclaims_disk() {
        let (bus, storage, kb, slug, tmp) = setup().await;
        let quarantine = tmp.path().join("quarantine");

        const N: usize = 100;
        let (tx, rx) = mpsc::channel::<WatchWork>(INGEST_QUEUE_CAPACITY);
        for i in 0..N {
            let p = tmp.path().join(format!("batch{i}.html"));
            std::fs::write(
                &p,
                format!("<html><title>Batch {i}</title><body>body {i}</body></html>"),
            )
            .unwrap();
            tx.send(WatchWork {
                kind: WatchKind::Created,
                path: p,
                force: false,
                keep_user_data: false,
            })
            .await
            .unwrap();
        }

        // Subscribe BEFORE the indexer starts draining so every
        // `artifact.indexed` is counted.
        let mut rx_events = bus.subscribe();

        let bus_for_indexer = bus.clone();
        let storage_for_indexer = storage.clone();
        let kb_for_indexer = kb.clone();
        let slug_for_indexer = slug.clone();
        let quarantine_for_indexer = quarantine.clone();
        let indexer = tokio::spawn(async move {
            run_with_ingest(
                kb_for_indexer,
                slug_for_indexer,
                storage_for_indexer,
                bus_for_indexer,
                quarantine_for_indexer,
                rx,
                None,
                None,
                None,
                crate::iframe::DEFAULT_HOST_SUFFIX.to_string(),
                crate::vcs::VersionsMode::Off,
                Arc::new(crate::metrics::PipelineMetrics::disabled()),
                false,
                crate::extmap::ExtensionMap::default(),
                crate::exclusions::IngestGate::default(),
                empty_dedup_cache(),
            )
            .await
        });
        drop(tx); // close the channel → run_with_ingest drains then returns

        timeout(Duration::from_secs(60), indexer)
            .await
            .expect("indexer did not drain + exit")
            .expect("indexer task panicked");

        // (a) every doc landed, and one `artifact.indexed` fired per doc.
        assert_eq!(storage.count_rows().await.unwrap() as usize, N);
        let mut indexed_count = 0;
        loop {
            match timeout(Duration::from_millis(200), rx_events.recv()).await {
                Ok(Ok(env)) if env.type_ == "artifact.indexed" => indexed_count += 1,
                Ok(Ok(_)) => continue,
                _ => break,
            }
        }
        assert_eq!(
            indexed_count, N,
            "one artifact.indexed event must fire per doc, batching or not",
        );

        // (b) far below N Lance manifest versions — proof the docs landed
        // via a handful of batched `upsert_docs` commits, not N singular
        // ones. N=100 docs at the 32-doc cap is AT MOST ceil(100/32)=4
        // upsert commits; a generous upper bound still fails outright on
        // the pre-GC-B7 one-commit-per-doc pattern (which would show ~100).
        let before = storage.dataset_stats().await.unwrap();
        assert!(
            before.versions <= 10,
            "expected far fewer than {N} manifest versions from batched \
             commits, got {}",
            before.versions
        );

        // (c) compact + physical reclaim (zero-window override — the
        // production 5-minute retention would see nothing eligible yet in a
        // fast test) shrinks the version count further, with every row
        // surviving intact.
        storage
            .compact_all_with_retention(0, true)
            .await
            .expect("compact_all_with_retention should succeed");
        let after = storage.dataset_stats().await.unwrap();
        assert!(
            after.versions < before.versions,
            "compact+reclaim must reduce manifest versions ({} -> {})",
            before.versions,
            after.versions
        );
        assert_eq!(
            storage.count_rows().await.unwrap() as usize,
            N,
            "row data must survive compaction + reclaim intact"
        );
    }

    /// SC1 — the v0.24 restart-with-backlog stall: the dedup pre-gate's
    /// mtime heal used to call `storage.touch_mtime` UNCONDITIONALLY on
    /// every dedup-skip, so every duplicate emission of an already-indexed
    /// unchanged file (the watcher's initial walk on EVERY restart, the
    /// periodic reconcile over a live backlog, overflow rescans) became a
    /// full-table-scan lance UPDATE + one manifest commit — O(N·fragments)
    /// quadratic, measured in the 20k scale lab as a collapse to ~1 doc/s.
    /// Pin the fix end-to-end through the REAL pipeline: a second indexer
    /// life (fresh in-memory cache, warmed from lance exactly as production
    /// does) re-fed the whole unchanged corpus must add ZERO lance commits.
    #[tokio::test]
    async fn restart_reemission_of_unchanged_corpus_adds_zero_lance_commits() {
        let (bus, storage, kb, slug, tmp) = setup().await;
        const N: usize = 40;
        let mut paths = Vec::with_capacity(N);
        for i in 0..N {
            let p = tmp.path().join(format!("re{i}.html"));
            std::fs::write(
                &p,
                format!("<html><title>Re {i}</title><body>body {i}</body></html>"),
            )
            .unwrap();
            paths.push(p);
        }

        // Pass 1 — cold index (first daemon life).
        let (tx, handle) = spawn_gated_indexer(
            &bus,
            &storage,
            &kb,
            &slug,
            &tmp,
            crate::exclusions::IngestGate::default(),
        );
        for p in &paths {
            tx.send(WatchWork {
                kind: WatchKind::Created,
                path: p.clone(),
                force: false,
                keep_user_data: false,
            })
            .await
            .unwrap();
        }
        drop(tx);
        timeout(Duration::from_secs(60), handle)
            .await
            .expect("pass-1 indexer did not drain + exit")
            .expect("pass-1 indexer panicked");
        assert_eq!(storage.count_rows().await.unwrap() as usize, N);

        let before = storage.dataset_stats().await.unwrap();

        // Pass 2 — simulated restart: a FRESH indexer (its dedup cache
        // warmed from `list_content_hashes`, hashes AND stored mtimes, the
        // production startup path) re-fed every file — exactly what
        // `initial_walk` emits on every daemon start.
        let (tx, handle) = spawn_gated_indexer(
            &bus,
            &storage,
            &kb,
            &slug,
            &tmp,
            crate::exclusions::IngestGate::default(),
        );
        for p in &paths {
            tx.send(WatchWork {
                kind: WatchKind::Created,
                path: p.clone(),
                force: false,
                keep_user_data: false,
            })
            .await
            .unwrap();
        }
        drop(tx);
        timeout(Duration::from_secs(60), handle)
            .await
            .expect("pass-2 indexer did not drain + exit")
            .expect("pass-2 indexer panicked");

        let after = storage.dataset_stats().await.unwrap();
        assert_eq!(
            after.versions, before.versions,
            "re-emitting an unchanged corpus must be write-free; the old \
             unconditional mtime heal added one touch_mtime commit PER FILE \
             ({} -> {} manifest versions)",
            before.versions, after.versions,
        );
        assert_eq!(
            storage.count_rows().await.unwrap() as usize,
            N,
            "row set untouched by the duplicate pass"
        );
    }

    /// SC1 — the heal's ORIGINAL purpose survives the conditional gate: a
    /// touched-but-unchanged file (mtime bumped, bytes identical — the
    /// `git checkout` shape) must still get its stored mtime healed, but
    /// exactly ONCE — even when the duplicate emission arrives twice in the
    /// same life (the cache learns the disk value after the heal) — and a
    /// later restart (cache re-warmed from lance, now carrying the healed
    /// mtime) must add zero further commits.
    #[tokio::test]
    async fn touched_but_unchanged_file_heals_mtime_exactly_once() {
        let (bus, storage, kb, slug, tmp) = setup().await;
        let p = tmp.path().join("touch.html");
        std::fs::write(
            &p,
            "<html><title>Touch</title><body>same bytes</body></html>",
        )
        .unwrap();

        let spawn_life = || {
            spawn_gated_indexer(
                &bus,
                &storage,
                &kb,
                &slug,
                &tmp,
                crate::exclusions::IngestGate::default(),
            )
        };

        // Pass 1 — index it.
        let (tx, handle) = spawn_life();
        tx.send(WatchWork {
            kind: WatchKind::Created,
            path: p.clone(),
            force: false,
            keep_user_data: false,
        })
        .await
        .unwrap();
        drop(tx);
        timeout(Duration::from_secs(30), handle)
            .await
            .expect("pass-1 drain")
            .expect("pass-1 panic");

        let rows = storage.list_content_hashes().await.unwrap();
        assert_eq!(rows.len(), 1);
        let stored0 = rows[0].2.expect("a freshly indexed row stores its mtime");

        // Bump the on-disk mtime WITHOUT changing bytes.
        let bumped = stored0 + 10;
        std::fs::File::options()
            .write(true)
            .open(&p)
            .unwrap()
            .set_modified(UNIX_EPOCH + Duration::from_secs(bumped as u64))
            .unwrap();

        let before = storage.dataset_stats().await.unwrap();

        // Pass 2 — restart; the SAME file emitted TWICE. First duplicate
        // heals (disk != stored), second is free (cache learned the value).
        let (tx, handle) = spawn_life();
        for _ in 0..2 {
            tx.send(WatchWork {
                kind: WatchKind::Modified,
                path: p.clone(),
                force: false,
                keep_user_data: false,
            })
            .await
            .unwrap();
        }
        drop(tx);
        timeout(Duration::from_secs(30), handle)
            .await
            .expect("pass-2 drain")
            .expect("pass-2 panic");

        let after = storage.dataset_stats().await.unwrap();
        assert_eq!(
            after.versions,
            before.versions + 1,
            "a genuine touch heals in exactly ONE commit (two duplicate \
             emissions must not double-heal)"
        );
        let rows = storage.list_content_hashes().await.unwrap();
        assert_eq!(
            rows[0].2,
            Some(bumped),
            "stored mtime healed to the bumped disk value"
        );
        assert_eq!(
            storage.count_rows().await.unwrap(),
            1,
            "no re-index — the byte-identity gate still holds"
        );

        // Pass 3 — another restart: the warm-up now carries the healed
        // mtime, so re-emitting the file is fully write-free.
        let (tx, handle) = spawn_life();
        tx.send(WatchWork {
            kind: WatchKind::Modified,
            path: p.clone(),
            force: false,
            keep_user_data: false,
        })
        .await
        .unwrap();
        drop(tx);
        timeout(Duration::from_secs(30), handle)
            .await
            .expect("pass-3 drain")
            .expect("pass-3 panic");
        let last = storage.dataset_stats().await.unwrap();
        assert_eq!(
            last.versions, after.versions,
            "the healed mtime persists across restarts — no further writes"
        );
    }

    /// GC-F2 — the GC-B7 fallback's error-isolation contract
    /// (`flush_prepared_batch`): the batched `upsert_docs` is ONE atomic
    /// Lance `merge_insert`, so a single poisoned doc fails the WHOLE batch
    /// without saying which doc sank it; the per-doc singular fallback must
    /// then (a) still land every healthy doc, (b) record the poison alone as
    /// a storage failure (errors table + `error` event, dedup cache left
    /// clean so its next watch event retries), and (c) return normally so
    /// the drain worker keeps processing subsequent batches.
    ///
    /// The poison is a dim-mismatched embedding — the exact stray the
    /// fallback's own comment names; that it fails `upsert_docs` is pinned
    /// at the storage level by
    /// `lance::upsert_with_wrong_dim_returns_storage_error_not_panic`. It
    /// cannot be staged through `run_with_ingest` end-to-end: with no
    /// embedder every prepared doc carries `embedding: None` (valid at any
    /// width), and a real wrong-dim embedder would poison every doc in the
    /// batch, not one. So each doc goes through the REAL `prepare_doc` and
    /// only the middle one's embedding is doctored — the same artifact a
    /// mismatched embedder output would produce — before the batch is
    /// flushed through the REAL `flush_prepared_batch`. The pre-flight
    /// assert proves the batched arm fails with the poison aboard, so every
    /// row that lands can ONLY have come through the fallback loop.
    #[tokio::test]
    async fn poisoned_doc_in_batch_falls_back_per_doc_and_only_it_fails() {
        let (bus, storage, kb, slug, tmp) = setup().await;
        let quarantine = tmp.path().join("quarantine");
        let metrics = crate::metrics::PipelineMetrics::disabled();
        let extensions = crate::extmap::ExtensionMap::default();
        let anchor_state: Arc<Mutex<HashMap<(String, String), crate::anchors::StaleAnchorEntry>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let indexed_hashes: DedupCache = Arc::new(Mutex::new(HashMap::new()));

        // Four real files through the REAL prepare stage (mirroring the
        // drain loop's per-file begin_run). Three form the poisoned batch;
        // the fourth is held back as the (c) follow-up batch.
        let mut prepared: Vec<PreparedDoc> = Vec::new();
        for i in 0..4 {
            let p = tmp.path().join(format!("iso{i}.html"));
            std::fs::write(
                &p,
                format!("<html><title>Iso {i}</title><body>body {i}</body></html>"),
            )
            .unwrap();
            let run_id = storage.begin_run(slug.clone(), unix_now()).await.unwrap();
            let doc = prepare_doc(
                &kb,
                &slug,
                tmp.path(),
                &storage,
                &bus,
                &quarantine,
                &p,
                ChangeKind::Created,
                false,
                None,
                &run_id,
                indexed_hashes.clone(),
                &metrics,
                false,
                &extensions,
                &crate::exclusions::IngestGate::default(),
            )
            .await
            .expect("prepare_doc must succeed for a healthy file")
            .expect("a fresh file must not hit the dedup gate");
            prepared.push(doc);
        }
        let followup = prepared.pop().unwrap();
        let healthy_ids = vec![
            prepared[0].artifact_id.as_str().to_string(),
            prepared[2].artifact_id.as_str().to_string(),
        ];
        let poison_id = prepared[1].artifact_id.as_str().to_string();
        let poison_path = prepared[1].path.clone();

        // Poison the MIDDLE doc: a 999-wide vector against the kb's fresh
        // 384-wide dataset (`Storage::open` with `config_dim: None`).
        prepared[1].doc.embedding = Some(vec![0.0; 999]);

        // Pre-flight: the batched arm MUST fail with the poison aboard —
        // otherwise this test never reaches the fallback branch.
        let batch_docs: Vec<Doc> = prepared.iter().map(|p| p.doc.clone()).collect();
        assert!(
            storage.upsert_docs(batch_docs).await.is_err(),
            "precondition: the atomic batch upsert must fail with a \
             dim-mismatched doc aboard",
        );
        assert_eq!(
            storage.count_rows().await.unwrap(),
            0,
            "the failed batch attempt must not commit any row",
        );

        // Subscribe BEFORE the flush so its event trail is captured.
        let mut rx_events = bus.subscribe();
        flush_prepared_batch(
            prepared,
            &kb,
            &slug,
            tmp.path(),
            &storage,
            &bus,
            &quarantine,
            None,
            anchor_state.clone(),
            indexed_hashes.clone(),
            crate::iframe::DEFAULT_HOST_SUFFIX,
            crate::vcs::VersionsMode::Off,
            &metrics,
            false,
        )
        .await;

        // (a) both healthy docs landed — only reachable via the singular
        // fallback (the batched arm was proven to fail above) — and the
        // poisoned row alone is absent.
        assert_eq!(
            storage.count_rows().await.unwrap(),
            2,
            "the two healthy docs must land despite the poisoned batch-mate",
        );
        for id in &healthy_ids {
            assert!(
                storage.get_by_id(id.clone()).await.unwrap().is_some(),
                "healthy doc {id} must be present after the fallback",
            );
        }
        assert!(
            storage
                .get_by_id(poison_id.clone())
                .await
                .unwrap()
                .is_none(),
            "the poisoned doc must NOT land",
        );

        // (b) the poison alone is recorded, with the fallback arm's message
        // shape — `format!("upsert: {e}")` exists only in that arm.
        let errs = storage.list_open_errors().await.unwrap();
        assert_eq!(errs.len(), 1, "exactly one recorded failure: {errs:?}");
        assert_eq!(errs[0].kind, "storage");
        assert_eq!(errs[0].path, poison_path);
        assert!(
            errs[0].message.starts_with("upsert:"),
            "must be the fallback's per-doc upsert failure, got: {}",
            errs[0].message
        );

        // The dedup cache holds ONLY the healthy hashes — the poison stays
        // retryable on its next watch event.
        {
            let cache = indexed_hashes.lock().unwrap();
            for id in &healthy_ids {
                assert!(cache.contains_key(id), "healthy {id} enters the cache");
            }
            assert!(
                !cache.contains_key(&poison_id),
                "a failed doc must not poison the dedup cache",
            );
        }

        // Event trail: one `artifact.indexed` per healthy doc, one `error`
        // (kind=storage) for the poison — the batching-or-not event
        // contract of the neighbouring test, minus the doc that failed.
        let mut indexed_ids: Vec<String> = Vec::new();
        let mut error_kinds: Vec<String> = Vec::new();
        loop {
            match timeout(Duration::from_millis(200), rx_events.recv()).await {
                Ok(Ok(env)) if env.type_ == "artifact.indexed" => indexed_ids.push(
                    env.payload["artifact_id"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                ),
                Ok(Ok(env)) if env.type_ == "error" => {
                    error_kinds.push(env.payload["kind"].as_str().unwrap_or_default().to_string())
                }
                Ok(Ok(_)) => continue,
                _ => break,
            }
        }
        indexed_ids.sort();
        let mut want = healthy_ids.clone();
        want.sort();
        assert_eq!(
            indexed_ids, want,
            "artifact.indexed fires for exactly the healthy docs",
        );
        assert_eq!(error_kinds, ["storage"], "one error event for the poison");

        // (c) no panic + the worker keeps going: a follow-up healthy batch
        // over the SAME storage lands cleanly after the poisoned one. (A
        // panic anywhere in the fallback would already have failed the
        // flush await above.)
        flush_prepared_batch(
            vec![followup],
            &kb,
            &slug,
            tmp.path(),
            &storage,
            &bus,
            &quarantine,
            None,
            anchor_state,
            indexed_hashes,
            crate::iframe::DEFAULT_HOST_SUFFIX,
            crate::vcs::VersionsMode::Off,
            &metrics,
            false,
        )
        .await;
        assert_eq!(
            storage.count_rows().await.unwrap(),
            3,
            "indexing must continue normally after a poisoned batch",
        );
    }

    // A fake kb-embedder that would succeed if it were ever invoked. Used by
    // `quarantined_doc_skips_embed_and_still_indexes` below: if the
    // quarantine gate is broken and an embed IS attempted, the doc ends up
    // WITH an embedding (a loud, immediate assertion failure) instead of the
    // test hanging on an unresponsive fake subprocess.
    #[cfg(unix)]
    const ALWAYS_OK_EMBEDDER: &str = r#"#!/bin/sh
printf '{"kind":"ready","model":"bge-small-en-v1.5","dim":3}\n'
while IFS= read -r line; do
  case "$line" in
    *'"kind":"shutdown"'*) exit 0 ;;
    *'"kind":"embed"'*)
      rid=$(printf '%s' "$line" | sed 's/.*"req_id":\([0-9][0-9]*\).*/\1/')
      printf '{"kind":"embed_ok","req_id":%s,"vectors":[[0.1,0.2,0.3]]}\n' "$rid"
      ;;
  esac
done
"#;

    /// 2026-08-21 ci-host incident hotfix, defect 3: before this fix, quarantine
    /// copied the offending file to the state dir but never gated FUTURE
    /// embed attempts — a 292MB session HTML kept getting a fresh doc-level
    /// embed on every hourly reconcile pass, OOM-looping the embedder (81
    /// quarantine events, RestartCount 198). Seeds the SAME `(path,
    /// content_hash)` failure state `record_failure` itself produces, then
    /// runs the doc through the REAL `prepare_doc` with a live (fake-
    /// subprocess) IPC embedder, and asserts: the doc still lands in the
    /// index (BM25/keyword-searchable — the embedding column is nullable by
    /// design), carries NO embedding, and the gated pass records no new
    /// error row (no counter inflation, no repeated quarantine copies).
    ///
    /// Seeding note: `record_error` dedups by `(path, content_hash)` — the
    /// FIRST call inserts `retry_count = 0`, and only LATER calls bump it
    /// (`storage/sqlite.rs::record_error`, pinned by
    /// `retry_count_returns_current_value`). So landing the STORED value at
    /// exactly `QUARANTINE_THRESHOLD` — the gate's own comparison — takes
    /// `QUARANTINE_THRESHOLD + 1` calls; this is the identical arithmetic
    /// `record_failure`'s own `retries + 1 >= QUARANTINE_THRESHOLD`
    /// quarantine check already relies on. The precondition assertion below
    /// pins the seeded value so a future change to that arithmetic fails
    /// here first, not via a confusing downstream assertion.
    #[cfg(unix)]
    #[tokio::test]
    async fn quarantined_doc_skips_embed_and_still_indexes() {
        use std::os::unix::fs::PermissionsExt;

        let (bus, storage, kb, slug, tmp) = setup().await;
        let quarantine = tmp.path().join("quarantine");
        let metrics = crate::metrics::PipelineMetrics::disabled();
        let extensions = crate::extmap::ExtensionMap::default();
        let indexed_hashes: DedupCache = Arc::new(Mutex::new(HashMap::new()));

        // `Embedder::spawn_ipc` is synchronous (blocking IO on a bounded
        // handshake thread) — no `.await` inside this block — so the
        // process-global `KB_EMBEDDER_BIN` mutation + the lock guarding it
        // stay entirely off the async call graph (clippy::await_holding_lock
        // forbids holding a `std::sync::Mutex` guard across an await point).
        // Shared with embed_ipc.rs's own fake-subprocess tests: both mutate
        // the SAME env var, so they must serialise on the SAME lock (see
        // that module's `ENV_LOCK` doc). The var is unset again before the
        // guard drops — nothing later in this test re-reads it (the gate
        // under test means the embedder is never actually invoked).
        let embedder: Arc<Mutex<Embedder>> = {
            let _env_guard = crate::embed_ipc::tests::ENV_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let script = tmp.path().join("always-ok-embedder.sh");
            std::fs::write(&script, ALWAYS_OK_EMBEDDER).unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
            std::env::set_var("KB_EMBEDDER_BIN", &script);
            let e = Embedder::spawn_ipc("bge-small-en-v1.5", tmp.path().to_path_buf(), 19).unwrap();
            std::env::remove_var("KB_EMBEDDER_BIN");
            Arc::new(Mutex::new(e))
        };

        let path = tmp.path().join("quarantined.html");
        std::fs::write(
            &path,
            "<html><title>Big</title><body>quarantine me</body></html>",
        )
        .unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let (_artifact_id, content_hash, _rel) = identity_for(&path, tmp.path(), &bytes);

        for i in 0..=QUARANTINE_THRESHOLD {
            storage
                .record_error(
                    "embed".to_string(),
                    slug.clone(),
                    path.clone(),
                    format!("embed failed: simulated OOM #{i}"),
                    Some(content_hash.clone()),
                    unix_now(),
                )
                .await
                .unwrap();
        }
        let seeded_retries = storage
            .retry_count_for_path_hash(path.clone(), content_hash.clone())
            .await
            .unwrap();
        assert_eq!(
            seeded_retries, QUARANTINE_THRESHOLD,
            "precondition: stored retry_count must reach QUARANTINE_THRESHOLD"
        );
        let errors_before = storage.list_open_errors().await.unwrap().len();

        // Subscribe BEFORE `prepare_doc` so its (synchronous) `index.
        // embed_skipped` emit is captured.
        let mut rx_events = bus.subscribe();

        let run_id = storage.begin_run(slug.clone(), unix_now()).await.unwrap();
        let prepared = prepare_doc(
            &kb,
            &slug,
            tmp.path(),
            &storage,
            &bus,
            &quarantine,
            &path,
            ChangeKind::Created,
            false,
            Some(&embedder),
            &run_id,
            indexed_hashes.clone(),
            &metrics,
            false,
            &extensions,
            &crate::exclusions::IngestGate::default(),
        )
        .await
        .expect("prepare_doc must succeed for an already-quarantined-but-readable file")
        .expect("a fresh dedup-cache entry must not hit the unchanged-content gate");

        assert!(
            prepared.doc.embedding.is_none(),
            "an already-quarantined doc must NOT get a fresh doc-level embedding"
        );
        let artifact_id_str = prepared.artifact_id.as_str().to_string();

        flush_prepared_batch(
            vec![prepared],
            &kb,
            &slug,
            tmp.path(),
            &storage,
            &bus,
            &quarantine,
            None,
            Arc::new(Mutex::new(HashMap::new())),
            indexed_hashes,
            crate::iframe::DEFAULT_HOST_SUFFIX,
            crate::vcs::VersionsMode::Off,
            &metrics,
            false,
        )
        .await;

        assert!(
            storage
                .get_by_id(artifact_id_str.clone())
                .await
                .unwrap()
                .is_some(),
            "the doc IS in the index despite the gated embed"
        );
        assert!(
            storage
                .embedding_by_id(artifact_id_str)
                .await
                .unwrap()
                .is_none(),
            "the persisted row must carry NO embedding"
        );

        let errors_after = storage.list_open_errors().await.unwrap().len();
        assert_eq!(
            errors_after, errors_before,
            "the gated pass must not record a new error row (no counter inflation)"
        );

        let saw_skip_event = timeout(Duration::from_millis(500), async {
            loop {
                match rx_events.recv().await {
                    Ok(env) if env.type_ == "index.embed_skipped" => return true,
                    Ok(_) => continue,
                    Err(_) => return false,
                }
            }
        })
        .await
        .unwrap_or(false);
        assert!(
            saw_skip_event,
            "index.embed_skipped must fire for the gated doc"
        );
    }

    #[tokio::test]
    async fn missing_file_emits_error_event() {
        let (bus, storage, kb, slug, tmp) = setup().await;
        let quarantine = tmp.path().join("quarantine");

        let indexer_rx = bus.subscribe();
        let bus_for_indexer = bus.clone();
        let storage_for_indexer = storage.clone();
        let kb_for_indexer = kb.clone();
        let slug_for_indexer = slug.clone();
        let quarantine_for_indexer = quarantine.clone();
        let _indexer = tokio::spawn(async move {
            run(
                kb_for_indexer,
                slug_for_indexer,
                storage_for_indexer,
                bus_for_indexer,
                quarantine_for_indexer,
                indexer_rx,
                None,
                None,
                crate::iframe::DEFAULT_HOST_SUFFIX.to_string(),
                crate::vcs::VersionsMode::Off,
                0,
                Vec::new(),
            )
            .await
        });

        let mut rx = bus.subscribe();
        let missing = tmp.path().join("does-not-exist.html");
        bus.emit(
            "watch.create",
            json!({"kb": kb.as_str(), "path": missing.to_string_lossy()}),
        );

        let env = timeout(Duration::from_secs(5), async {
            loop {
                match rx.recv().await {
                    Ok(env) if env.type_ == "error" => return env,
                    _ => continue,
                }
            }
        })
        .await
        .expect("error event timeout");
        assert_eq!(env.payload["kind"].as_str().unwrap(), "io");
        assert_eq!(env.payload["kb"].as_str().unwrap(), "smoke");
    }

    #[tokio::test]
    async fn other_kb_events_are_ignored() {
        let (bus, storage, kb, slug, tmp) = setup().await;
        let quarantine = tmp.path().join("quarantine");

        let indexer_rx = bus.subscribe();
        let bus_for_indexer = bus.clone();
        let storage_for_indexer = storage.clone();
        let kb_for_indexer = kb.clone();
        let slug_for_indexer = slug.clone();
        let quarantine_for_indexer = quarantine.clone();
        let _indexer = tokio::spawn(async move {
            run(
                kb_for_indexer,
                slug_for_indexer,
                storage_for_indexer,
                bus_for_indexer,
                quarantine_for_indexer,
                indexer_rx,
                None,
                None,
                crate::iframe::DEFAULT_HOST_SUFFIX.to_string(),
                crate::vcs::VersionsMode::Off,
                0,
                Vec::new(),
            )
            .await
        });

        // Emit for a DIFFERENT kb.
        bus.emit(
            "watch.create",
            json!({"kb": "other-kb", "path": "/tmp/whatever.html"}),
        );

        // Give the indexer a beat to (not) react.
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(storage.count_rows().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn run_lifecycle_emits_start_file_complete() {
        let (bus, storage, kb, slug, tmp) = setup().await;
        let quarantine = tmp.path().join("quarantine");

        let indexer_rx = bus.subscribe();
        let bus_for_indexer = bus.clone();
        let storage_for_indexer = storage.clone();
        let kb_for_indexer = kb.clone();
        let slug_for_indexer = slug.clone();
        let quarantine_for_indexer = quarantine.clone();
        let _indexer = tokio::spawn(async move {
            run(
                kb_for_indexer,
                slug_for_indexer,
                storage_for_indexer,
                bus_for_indexer,
                quarantine_for_indexer,
                indexer_rx,
                None,
                None,
                crate::iframe::DEFAULT_HOST_SUFFIX.to_string(),
                crate::vcs::VersionsMode::Off,
                0,
                Vec::new(),
            )
            .await
        });

        let html_path = tmp.path().join("a.html");
        std::fs::write(&html_path, "<html><title>X</title></html>").unwrap();
        let mut rx = bus.subscribe();

        bus.emit(
            "watch.create",
            json!({"kb": kb.as_str(), "path": html_path.to_string_lossy()}),
        );

        let mut saw_start = false;
        let mut saw_file = false;
        let mut saw_complete = false;

        timeout(Duration::from_secs(5), async {
            loop {
                match rx.recv().await {
                    Ok(env) => match env.type_.as_str() {
                        "index.start" => saw_start = true,
                        "index.file" => saw_file = true,
                        "index.complete" => {
                            saw_complete = true;
                            break;
                        }
                        _ => {}
                    },
                    Err(_) => continue,
                }
            }
        })
        .await
        .unwrap();

        assert!(saw_start);
        assert!(saw_file);
        assert!(saw_complete);
    }

    // --- v0.3 G2 anchor-stale --------------------------------------------

    /// Block until an `artifact.indexed` envelope for `artifact_id` arrives
    /// (5s cap). The sidecar write precedes that emit, so the caller can
    /// inspect the persisted anchor-stale set immediately after.
    async fn wait_for_indexed(
        rx: &mut tokio::sync::broadcast::Receiver<crate::types::Envelope>,
        artifact_id: &str,
    ) {
        timeout(Duration::from_secs(5), async {
            loop {
                match rx.recv().await {
                    Ok(env)
                        if env.type_ == "artifact.indexed"
                            && env.payload["artifact_id"].as_str() == Some(artifact_id) =>
                    {
                        return
                    }
                    _ => continue,
                }
            }
        })
        .await
        .expect("artifact.indexed never fired");
    }

    #[tokio::test]
    async fn comment_anchor_stale_fires_when_section_id_disappears() {
        use crate::review::{Anchor, Author, Comment, CommentStatus, ReviewFile};
        use chrono::Utc;
        let (bus, storage, kb, slug, tmp) = setup().await;
        let quarantine = tmp.path().join("quarantine");
        let review_dir = tmp.path().join(".review");
        std::fs::create_dir_all(&review_dir).unwrap();

        // The artifact_id is the source-relative path. Drop a review
        // file at <id>.json, index the HTML, and assert the open
        // comment's anchor (which doesn't bind to this content) fires
        // `comment.anchor_stale`.
        let html_path = tmp.path().join("regen.html");
        let final_html = "<html><body><section id=\"new-id\">\
             <h2>Rewritten</h2><p>different text entirely</p></section></body></html>";
        std::fs::write(&html_path, final_html).unwrap();
        let id = crate::ids::ArtifactId::from_path("regen.html");

        let mut review = ReviewFile::empty_skeleton(&kb, id.as_str(), "regen");
        review.comments.push(Comment {
            id: "c_stale".into(),
            status: CommentStatus::Open,
            file: id.as_str().to_string(),
            file_label: "main".into(),
            anchor: Anchor::Section {
                id: "vanishing-id".into(),
                tag: Some("section".into()),
                snippet: None,
            },
            author: Author::You,
            body: "anchor binds to a section the new HTML doesn't have".into(),
            created_at: Utc::now(),
            edited_at: None,
            replies: vec![],
            choices: vec![],
            attachments: vec![],
            user: None,
        });
        let review_path = review_dir.join(format!("{}.json", id.as_str()));
        crate::review::save_atomic(&review_path, &review, None).unwrap();

        // Spawn the indexer with the review_dir wired.
        let indexer_rx = bus.subscribe();
        let bus_for_indexer = bus.clone();
        let storage_for_indexer = storage.clone();
        let kb_for_indexer = kb.clone();
        let slug_for_indexer = slug.clone();
        let quarantine_for_indexer = quarantine.clone();
        let review_for_indexer = Some(review_dir.clone());
        let _indexer = tokio::spawn(async move {
            run(
                kb_for_indexer,
                slug_for_indexer,
                storage_for_indexer,
                bus_for_indexer,
                quarantine_for_indexer,
                indexer_rx,
                None,
                review_for_indexer,
                crate::iframe::DEFAULT_HOST_SUFFIX.to_string(),
                crate::vcs::VersionsMode::Off,
                0,
                Vec::new(),
            )
            .await
        });

        let mut rx = bus.subscribe();
        bus.emit(
            "watch.create",
            json!({"kb": kb.as_str(), "path": html_path.to_string_lossy()}),
        );

        let stale = timeout(Duration::from_secs(5), async {
            loop {
                match rx.recv().await {
                    Ok(env) if env.type_ == "comment.anchor_stale" => return env,
                    _ => continue,
                }
            }
        })
        .await
        .expect("comment.anchor_stale never fired");
        assert_eq!(stale.payload["kb"].as_str(), Some(kb.as_str()));
        assert_eq!(stale.payload["comment_id"].as_str(), Some("c_stale"));
        assert_eq!(stale.payload["artifact_id"].as_str(), Some(id.as_str()));
        // Track U — the event carries the source-relative path so the
        // SPA dashboard can build the path permalink from a live event.
        assert_eq!(
            stale.payload["source_relative"].as_str(),
            Some("regen.html")
        );
        // Storage should also have ingested the new doc.
        assert!(storage.count_rows().await.unwrap() >= 1);
    }

    #[tokio::test]
    async fn deleted_comment_pruned_from_anchor_sidecar_on_reindex() {
        // R5 — when a comment is deleted (the DELETE handler removes it
        // from the review file + prunes the on-disk sidecar), the next
        // reindex must NOT resurrect its stale key from the in-memory
        // anchor_state map. The self-heal `retain` drops keys for comments
        // no longer in the file.
        use crate::review::{Anchor, Author, Comment, CommentStatus, ReviewFile};
        use chrono::Utc;
        let (bus, storage, kb, slug, tmp) = setup().await;
        let quarantine = tmp.path().join("quarantine");
        let review_dir = tmp.path().join(".review");
        std::fs::create_dir_all(&review_dir).unwrap();

        let html_path = tmp.path().join("prune.html");
        std::fs::write(
            &html_path,
            "<html><body><section id=\"here\"><h2>One</h2><p>first</p></section></body></html>",
        )
        .unwrap();
        let id = crate::ids::ArtifactId::from_path("prune.html");

        // One open comment whose section anchor doesn't bind → goes stale.
        let mut review = ReviewFile::empty_skeleton(&kb, id.as_str(), "prune");
        review.comments.push(Comment {
            id: "c_stale".into(),
            status: CommentStatus::Open,
            file: id.as_str().to_string(),
            file_label: "main".into(),
            anchor: Anchor::Section {
                id: "no-such-id".into(),
                tag: Some("section".into()),
                snippet: None,
            },
            author: Author::You,
            body: "stale".into(),
            created_at: Utc::now(),
            edited_at: None,
            replies: vec![],
            choices: vec![],
            attachments: vec![],
            user: None,
        });
        let review_path = review_dir.join(format!("{}.json", id.as_str()));
        crate::review::save_atomic(&review_path, &review, None).unwrap();

        let indexer_rx = bus.subscribe();
        let bus_i = bus.clone();
        let storage_i = storage.clone();
        let kb_i = kb.clone();
        let slug_i = slug.clone();
        let quarantine_i = quarantine.clone();
        let review_i = Some(review_dir.clone());
        let _indexer = tokio::spawn(async move {
            run(
                kb_i,
                slug_i,
                storage_i,
                bus_i,
                quarantine_i,
                indexer_rx,
                None,
                review_i,
                crate::iframe::DEFAULT_HOST_SUFFIX.to_string(),
                crate::vcs::VersionsMode::Off,
                0,
                Vec::new(),
            )
            .await
        });

        let mut rx = bus.subscribe();
        let sidecar = crate::anchors::sidecar_path(&review_dir);
        let key = (id.as_str().to_string(), "c_stale".to_string());

        // First index → the comment goes stale → sidecar holds the key.
        bus.emit(
            "watch.create",
            json!({"kb": kb.as_str(), "path": html_path.to_string_lossy()}),
        );
        wait_for_indexed(&mut rx, id.as_str()).await;
        assert!(
            crate::anchors::load(&sidecar).contains_key(&key),
            "sidecar should hold the stale key after first index"
        );

        // Simulate the DELETE handler: drop the comment from the review
        // file. Change the HTML too so the content-hash dedup gate doesn't
        // skip the reindex (the self-heal only runs when index_one runs).
        crate::review::save_atomic(
            &review_path,
            &ReviewFile::empty_skeleton(&kb, id.as_str(), "prune"),
            None,
        )
        .unwrap();
        std::fs::write(
            &html_path,
            "<html><body><section id=\"here\"><h2>Two</h2><p>second body</p></section></body></html>",
        )
        .unwrap();

        bus.emit(
            "watch.modify",
            json!({"kb": kb.as_str(), "path": html_path.to_string_lossy()}),
        );
        wait_for_indexed(&mut rx, id.as_str()).await;
        assert!(
            !crate::anchors::load(&sidecar).contains_key(&key),
            "self-heal must prune the deleted comment's stale key on reindex"
        );
    }

    #[tokio::test]
    async fn legacy_content_hash_review_file_migrates_to_path_based_id() {
        // v0.7.1 H2 — a pre-v0.7 review file named `<content-hash>.json`
        // is renamed to `<path-id>.json` on the next index, and its
        // comments still load. Proof: an open comment with a dead anchor
        // fires `comment.anchor_stale` — which only happens if the
        // migrated file was actually read.
        use crate::review::{Anchor, Author, Comment, CommentStatus, ReviewFile};
        use chrono::Utc;
        let (bus, storage, kb, slug, tmp) = setup().await;
        let quarantine = tmp.path().join("quarantine");
        let review_dir = tmp.path().join(".review");
        std::fs::create_dir_all(&review_dir).unwrap();

        let html_path = tmp.path().join("legacy.html");
        let html = "<html><body><section id=\"new\"><h2>Now</h2>\
             <p>fresh text</p></section></body></html>";
        std::fs::write(&html_path, html).unwrap();
        let path_id = crate::ids::ArtifactId::from_path("legacy.html");
        let content_hash = crate::ids::ArtifactId::from_html_bytes(html.as_bytes());
        assert_ne!(path_id.as_str(), content_hash.as_str());

        // Drop the review file under the OLD content-hash name.
        let mut review = ReviewFile::empty_skeleton(&kb, content_hash.as_str(), "legacy");
        review.comments.push(Comment {
            id: "c_legacy".into(),
            status: CommentStatus::Open,
            file: content_hash.as_str().to_string(),
            file_label: "main".into(),
            anchor: Anchor::Section {
                id: "gone".into(),
                tag: Some("section".into()),
                snippet: None,
            },
            author: Author::You,
            body: "anchored to a section the current HTML lacks".into(),
            created_at: Utc::now(),
            edited_at: None,
            replies: vec![],
            choices: vec![],
            attachments: vec![],
            user: None,
        });
        let legacy_path = review_dir.join(format!("{}.json", content_hash.as_str()));
        crate::review::save_atomic(&legacy_path, &review, None).unwrap();

        let indexer_rx = bus.subscribe();
        let _indexer = tokio::spawn(run(
            kb.clone(),
            slug.clone(),
            storage.clone(),
            bus.clone(),
            quarantine.clone(),
            indexer_rx,
            None,
            Some(review_dir.clone()),
            crate::iframe::DEFAULT_HOST_SUFFIX.to_string(),
            crate::vcs::VersionsMode::Off,
            0,
            Vec::new(),
        ));

        let mut rx = bus.subscribe();
        bus.emit(
            "watch.create",
            json!({"kb": kb.as_str(), "path": html_path.to_string_lossy()}),
        );

        let stale = timeout(Duration::from_secs(5), async {
            loop {
                match rx.recv().await {
                    Ok(env) if env.type_ == "comment.anchor_stale" => return env,
                    _ => continue,
                }
            }
        })
        .await
        .expect("comment.anchor_stale never fired — migration likely didn't run");
        assert_eq!(stale.payload["comment_id"].as_str(), Some("c_legacy"));
        assert_eq!(
            stale.payload["artifact_id"].as_str(),
            Some(path_id.as_str())
        );

        // Filesystem: renamed to the path-based id, legacy name gone.
        assert!(
            review_dir
                .join(format!("{}.json", path_id.as_str()))
                .exists(),
            "review file should now be keyed on the path-based id"
        );
        assert!(
            !legacy_path.exists(),
            "legacy content-hash file should be gone after migration"
        );
    }

    #[tokio::test]
    async fn comment_anchor_stale_skipped_when_resolved() {
        // A `Resolved` comment must NOT trigger anchor_stale even when
        // its anchor wouldn't bind to the current HTML.
        use crate::review::{Anchor, Author, Comment, CommentStatus, ReviewFile};
        use chrono::Utc;
        let (bus, storage, kb, slug, tmp) = setup().await;
        let quarantine = tmp.path().join("quarantine");
        let review_dir = tmp.path().join(".review");
        std::fs::create_dir_all(&review_dir).unwrap();

        let html_path = tmp.path().join("plain.html");
        let html = "<html><body><p>hello</p></body></html>";
        std::fs::write(&html_path, html).unwrap();
        let id = crate::ids::ArtifactId::from_path("plain.html");

        let mut review = ReviewFile::empty_skeleton(&kb, id.as_str(), "plain");
        review.comments.push(Comment {
            id: "c_resolved".into(),
            status: CommentStatus::Resolved,
            file: id.as_str().to_string(),
            file_label: "main".into(),
            anchor: Anchor::Section {
                id: "no-such-id".into(),
                tag: None,
                snippet: None,
            },
            author: Author::You,
            body: "resolved comments shouldn't fire anchor_stale".into(),
            created_at: Utc::now(),
            edited_at: None,
            replies: vec![],
            choices: vec![],
            attachments: vec![],
            user: None,
        });
        let review_path = review_dir.join(format!("{}.json", id.as_str()));
        crate::review::save_atomic(&review_path, &review, None).unwrap();

        let indexer_rx = bus.subscribe();
        let bus_for_indexer = bus.clone();
        let storage_for_indexer = storage.clone();
        let kb_for_indexer = kb.clone();
        let slug_for_indexer = slug.clone();
        let quarantine_for_indexer = quarantine.clone();
        let review_for_indexer = Some(review_dir.clone());
        let _indexer = tokio::spawn(async move {
            run(
                kb_for_indexer,
                slug_for_indexer,
                storage_for_indexer,
                bus_for_indexer,
                quarantine_for_indexer,
                indexer_rx,
                None,
                review_for_indexer,
                crate::iframe::DEFAULT_HOST_SUFFIX.to_string(),
                crate::vcs::VersionsMode::Off,
                0,
                Vec::new(),
            )
            .await
        });

        let mut rx = bus.subscribe();
        bus.emit(
            "watch.create",
            json!({"kb": kb.as_str(), "path": html_path.to_string_lossy()}),
        );

        let result = timeout(Duration::from_millis(800), async {
            loop {
                match rx.recv().await {
                    Ok(env) if env.type_ == "comment.anchor_stale" => return Some(env),
                    Ok(env) if env.type_ == "index.complete" => return None,
                    _ => continue,
                }
            }
        })
        .await
        .ok()
        .flatten();
        assert!(
            result.is_none(),
            "resolved comment must not fire anchor_stale"
        );
    }

    #[tokio::test]
    async fn no_anchor_stale_when_review_dir_is_none() {
        // Smoke: when the indexer is spawned without a review_dir, no
        // comment.anchor_stale should ever fire even if comments exist
        // on disk somewhere else.
        let (bus, storage, kb, slug, tmp) = setup().await;
        let quarantine = tmp.path().join("quarantine");
        let html_path = tmp.path().join("plain.html");
        std::fs::write(&html_path, "<html><body><p>hi</p></body></html>").unwrap();

        let indexer_rx = bus.subscribe();
        let bus_for_indexer = bus.clone();
        let storage_for_indexer = storage.clone();
        let kb_for_indexer = kb.clone();
        let slug_for_indexer = slug.clone();
        let quarantine_for_indexer = quarantine.clone();
        let _indexer = tokio::spawn(async move {
            run(
                kb_for_indexer,
                slug_for_indexer,
                storage_for_indexer,
                bus_for_indexer,
                quarantine_for_indexer,
                indexer_rx,
                None,
                None, // <- review_dir = None
                crate::iframe::DEFAULT_HOST_SUFFIX.to_string(),
                crate::vcs::VersionsMode::Off,
                0,
                Vec::new(),
            )
            .await
        });

        let mut rx = bus.subscribe();
        bus.emit(
            "watch.create",
            json!({"kb": kb.as_str(), "path": html_path.to_string_lossy()}),
        );

        // Wait long enough for a hypothetical anchor_stale to land.
        let result = timeout(Duration::from_millis(800), async {
            loop {
                match rx.recv().await {
                    Ok(env) if env.type_ == "comment.anchor_stale" => return Some(env),
                    Ok(env) if env.type_ == "index.complete" => return None,
                    _ => continue,
                }
            }
        })
        .await
        .ok()
        .flatten();
        assert!(result.is_none(), "expected no comment.anchor_stale");
    }

    // --- v0.5 P4 — anchor_resolved transition ----------------------------

    #[tokio::test]
    async fn comment_anchor_resolved_fires_on_stale_to_exact_transition() {
        // Reindex 1 — HTML missing the section → emits anchor_stale.
        // Reindex 2 — HTML has the section restored → emits anchor_resolved.
        // The in-process tracker carries the "was stale" state between
        // the two passes.
        use crate::review::{Anchor, Author, Comment, CommentStatus, ReviewFile};
        use chrono::Utc;
        let (bus, _storage, kb, slug, tmp) = setup().await;
        let quarantine = tmp.path().join("quarantine");
        let review_dir = tmp.path().join(".review");
        std::fs::create_dir_all(&review_dir).unwrap();

        let html_path = tmp.path().join("toggle.html");
        // Two HTML states for the same file. The artifact id is the
        // source-relative path, so it's the SAME id across both passes
        // — one review file covers both.
        let html_missing = "<html><body><p>no section here</p></body></html>";
        let html_present =
            "<html><body><section id=\"appendix\"><p>back!</p></section></body></html>";
        let id = crate::ids::ArtifactId::from_path("toggle.html");

        // Drop a review file at <id>.json so the anchor resolution
        // flips Stale → Exact when the HTML regains the section.
        let mut review = ReviewFile::empty_skeleton(&kb, id.as_str(), "toggle");
        review.comments.push(Comment {
            id: "c_toggle".into(),
            status: CommentStatus::Open,
            file: id.as_str().to_string(),
            file_label: "main".into(),
            anchor: Anchor::Section {
                id: "appendix".into(),
                tag: Some("section".into()),
                snippet: None,
            },
            author: Author::You,
            body: "anchor toggles between stale and resolved".into(),
            created_at: Utc::now(),
            edited_at: None,
            replies: vec![],
            choices: vec![],
            attachments: vec![],
            user: None,
        });
        let review_path = review_dir.join(format!("{}.json", id.as_str()));
        crate::review::save_atomic(&review_path, &review, None).unwrap();

        let indexer_rx = bus.subscribe();
        let bus_for_indexer = bus.clone();
        let storage_for_indexer = _storage.clone();
        let kb_for_indexer = kb.clone();
        let slug_for_indexer = slug.clone();
        let quarantine_for_indexer = quarantine.clone();
        let review_for_indexer = Some(review_dir.clone());
        let _indexer = tokio::spawn(async move {
            run(
                kb_for_indexer,
                slug_for_indexer,
                storage_for_indexer,
                bus_for_indexer,
                quarantine_for_indexer,
                indexer_rx,
                None,
                review_for_indexer,
                crate::iframe::DEFAULT_HOST_SUFFIX.to_string(),
                crate::vcs::VersionsMode::Off,
                0,
                Vec::new(),
            )
            .await
        });

        let mut rx = bus.subscribe();

        // PASS 1: missing section. Expect anchor_stale.
        std::fs::write(&html_path, html_missing).unwrap();
        bus.emit(
            "watch.create",
            json!({"kb": kb.as_str(), "path": html_path.to_string_lossy()}),
        );
        let stale = timeout(Duration::from_secs(5), async {
            loop {
                match rx.recv().await {
                    Ok(env) if env.type_ == "comment.anchor_stale" => return env,
                    _ => continue,
                }
            }
        })
        .await
        .expect("anchor_stale never fired on pass 1");
        assert_eq!(stale.payload["comment_id"].as_str(), Some("c_toggle"));

        // PASS 2: section restored. Expect anchor_resolved.
        std::fs::write(&html_path, html_present).unwrap();
        bus.emit(
            "watch.modify",
            json!({"kb": kb.as_str(), "path": html_path.to_string_lossy()}),
        );
        let resolved = timeout(Duration::from_secs(5), async {
            loop {
                match rx.recv().await {
                    Ok(env) if env.type_ == "comment.anchor_resolved" => return env,
                    _ => continue,
                }
            }
        })
        .await
        .expect("anchor_resolved never fired on pass 2");
        assert_eq!(resolved.payload["comment_id"].as_str(), Some("c_toggle"));
        assert_eq!(resolved.payload["kb"].as_str(), Some(kb.as_str()));
    }

    // --- W6 — memory-session anchor-resolution fix (moonshots M2 / memo R2) ---

    /// Minimal 3-entity escape mirroring `kb-capture.sh`'s `sed` pipeline —
    /// enough to build a synthetic capture doc inline for these tests
    /// without reaching for the real fixture corpus (`sessions/view.rs`'s
    /// unit tests already exercise those).
    fn escape_for_pre(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }

    /// Build a minimal but real memory-session capture: one typed user
    /// prompt, one assistant prose reply, both carrying a `uuid` so the
    /// engine mints real `t-<uuid12>` turn ids.
    fn synthetic_capture_html() -> String {
        let jsonl = concat!(
            r#"{"type":"user","uuid":"11112222-3333-4444-5555-666677778888","timestamp":"2026-07-01T00:00:00Z","sessionId":"anchor-fix-test","message":{"role":"user","content":"turn-anchor fix test prompt"},"promptSource":"typed"}"#,
            "\n",
            r#"{"type":"assistant","uuid":"99998888-7777-6666-5555-444433332222","timestamp":"2026-07-01T00:00:05Z","sessionId":"anchor-fix-test","message":{"role":"assistant","model":"claude","content":[{"type":"text","text":"turn-anchor fix test reply"}]}}"#,
        );
        format!(
            "<html><head><meta name=\"kb-category\" content=\"memory-session\">\
             <meta name=\"kb-session\" content=\"anchor-fix-test\"></head>\
             <body><h1>anchor fix test</h1><pre>{}</pre></body></html>",
            escape_for_pre(jsonl)
        )
    }

    /// FAILING-FIRST PROOF (Section scope), exercised through the REAL
    /// indexer pipeline: before the category-gated branch in
    /// `finish_indexed_doc`, EVERY comment anchor — session captures
    /// included — resolved via `review::fuzzy_resolve_anchor_with(html, …)`
    /// against the raw `<pre>{escaped JSONL}</pre>` bytes. A
    /// `Section{id:"t-<uuid12>"}` id is byte-absent from those bytes (the
    /// renderer mints it at serve time from the record's own `uuid`), so
    /// EVERY reindex would flip this comment to Stale and this test's
    /// "no anchor_stale before artifact.indexed" assertion would fail. The
    /// fix (`is_memory_session` → `sessions::view::resolve_capture_anchor`)
    /// makes it resolve Fresh instead.
    #[tokio::test]
    async fn memory_session_section_turn_anchor_resolves_fresh_not_stale() {
        use crate::review::{Anchor, Author, Comment, CommentStatus, ReviewFile};
        use chrono::Utc;

        let (bus, _storage, kb, slug, tmp) = setup().await;
        let quarantine = tmp.path().join("quarantine");
        let review_dir = tmp.path().join(".review");
        std::fs::create_dir_all(&review_dir).unwrap();

        let html = synthetic_capture_html();
        // The REAL turn id the engine mints for the user turn — computed via
        // the exact function the fix calls, no hand-rolled uuid math.
        let view = crate::sessions::view::session_view_for_capture_html(&html);
        let turn_id = view
            .turns
            .first()
            .expect("synthetic capture has a user turn")
            .id
            .clone();
        assert!(turn_id.starts_with("t-"));

        let html_path = tmp.path().join("turn-anchor.html");
        std::fs::write(&html_path, &html).unwrap();
        let id = crate::ids::ArtifactId::from_path("turn-anchor.html");

        let mut review = ReviewFile::empty_skeleton(&kb, id.as_str(), "turn-anchor");
        review.comments.push(Comment {
            id: "c_turn".into(),
            status: CommentStatus::Open,
            file: id.as_str().to_string(),
            file_label: "main".into(),
            anchor: Anchor::Section {
                id: turn_id,
                tag: None,
                snippet: None,
            },
            author: Author::You,
            body: "flag this turn for the next session".into(),
            created_at: Utc::now(),
            edited_at: None,
            replies: vec![],
            choices: vec![],
            attachments: vec![],
            user: None,
        });
        let review_path = review_dir.join(format!("{}.json", id.as_str()));
        crate::review::save_atomic(&review_path, &review, None).unwrap();

        let indexer_rx = bus.subscribe();
        let _indexer = tokio::spawn(run(
            kb.clone(),
            slug.clone(),
            _storage.clone(),
            bus.clone(),
            quarantine.clone(),
            indexer_rx,
            None,
            Some(review_dir.clone()),
            crate::iframe::DEFAULT_HOST_SUFFIX.to_string(),
            crate::vcs::VersionsMode::Off,
            0,
            Vec::new(),
        ));

        let mut rx = bus.subscribe();
        bus.emit(
            "watch.create",
            json!({"kb": kb.as_str(), "path": html_path.to_string_lossy()}),
        );
        let mut saw_stale = false;
        timeout(Duration::from_secs(5), async {
            loop {
                match rx.recv().await {
                    Ok(env) if env.type_ == "comment.anchor_stale" => {
                        saw_stale = true;
                    }
                    Ok(env)
                        if env.type_ == "artifact.indexed"
                            && env.payload["artifact_id"].as_str() == Some(id.as_str()) =>
                    {
                        return;
                    }
                    Ok(_) => continue,
                    Err(_) => return,
                }
            }
        })
        .await
        .expect("artifact.indexed never fired");
        assert!(
            !saw_stale,
            "Section{{id: t-<uuid12>}} anchor on a memory-session capture must \
             resolve Fresh — comment.anchor_stale fired (the pre-fix bug)"
        );
    }

    /// FAILING-FIRST PROOF (Selection scope), same shape as the Section
    /// test above: a Selection anchor quoting the assistant's RENDERED
    /// prose is byte-absent from the raw escaped `<pre>` (it's wrapped
    /// inside a JSON `content` string, not bare paragraph text — a
    /// capture's raw HTML has no `<p>`/`<li>`/… elements at all for the
    /// generic block-level DOM resolver to scan), so it too would flap
    /// Stale on every reindex before this fix.
    #[tokio::test]
    async fn memory_session_selection_anchor_resolves_fresh_not_stale() {
        use crate::review::{Anchor, Author, Comment, CommentStatus, ReviewFile};
        use chrono::Utc;

        let (bus, _storage, kb, slug, tmp) = setup().await;
        let quarantine = tmp.path().join("quarantine");
        let review_dir = tmp.path().join(".review");
        std::fs::create_dir_all(&review_dir).unwrap();

        let html = synthetic_capture_html();
        let html_path = tmp.path().join("selection-anchor.html");
        std::fs::write(&html_path, &html).unwrap();
        let id = crate::ids::ArtifactId::from_path("selection-anchor.html");

        let mut review = ReviewFile::empty_skeleton(&kb, id.as_str(), "selection-anchor");
        review.comments.push(Comment {
            id: "c_sel".into(),
            status: CommentStatus::Open,
            file: id.as_str().to_string(),
            file_label: "main".into(),
            anchor: Anchor::Selection {
                css_path: "body > main:nth-of-type(1) > p:nth-of-type(1)".into(),
                offset: 0,
                snippet: "turn-anchor fix test reply".into(),
            },
            author: Author::You,
            body: "quoting the assistant's reply".into(),
            created_at: Utc::now(),
            edited_at: None,
            replies: vec![],
            choices: vec![],
            attachments: vec![],
            user: None,
        });
        let review_path = review_dir.join(format!("{}.json", id.as_str()));
        crate::review::save_atomic(&review_path, &review, None).unwrap();

        let indexer_rx = bus.subscribe();
        let _indexer = tokio::spawn(run(
            kb.clone(),
            slug.clone(),
            _storage.clone(),
            bus.clone(),
            quarantine.clone(),
            indexer_rx,
            None,
            Some(review_dir.clone()),
            crate::iframe::DEFAULT_HOST_SUFFIX.to_string(),
            crate::vcs::VersionsMode::Off,
            0,
            Vec::new(),
        ));

        let mut rx = bus.subscribe();
        bus.emit(
            "watch.create",
            json!({"kb": kb.as_str(), "path": html_path.to_string_lossy()}),
        );
        let mut saw_stale = false;
        timeout(Duration::from_secs(5), async {
            loop {
                match rx.recv().await {
                    Ok(env) if env.type_ == "comment.anchor_stale" => {
                        saw_stale = true;
                    }
                    Ok(env)
                        if env.type_ == "artifact.indexed"
                            && env.payload["artifact_id"].as_str() == Some(id.as_str()) =>
                    {
                        return;
                    }
                    Ok(_) => continue,
                    Err(_) => return,
                }
            }
        })
        .await
        .expect("artifact.indexed never fired");
        assert!(
            !saw_stale,
            "Selection anchor quoting rendered prose on a memory-session \
             capture must resolve Fresh — comment.anchor_stale fired (the \
             pre-fix bug)"
        );
    }

    /// PIN: `is_memory_session` gates the WHOLE new resolution rule — an
    /// ordinary (non-`memory-session`) artifact must keep resolving through
    /// `review::fuzzy_resolve_anchor_with` exactly as before, even when its
    /// section id happens to look like a turn id. If the category gate were
    /// ever inverted or dropped, this doc would start resolving through
    /// `sessions::view::resolve_capture_anchor` instead — which parses no
    /// `<pre>` JSONL from this doc at all, finds zero turns, and would call
    /// the SAME id Stale — so this test also acts as a regression trap for
    /// the gate itself, not just documentation of intent.
    #[tokio::test]
    async fn non_session_doc_section_anchor_still_uses_generic_resolver() {
        use crate::review::{Anchor, Author, Comment, CommentStatus, ReviewFile};
        use chrono::Utc;

        let (bus, _storage, kb, slug, tmp) = setup().await;
        let quarantine = tmp.path().join("quarantine");
        let review_dir = tmp.path().join(".review");
        std::fs::create_dir_all(&review_dir).unwrap();

        // No kb-category meta — an ordinary doc. The section id LOOKS like a
        // turn id but is a real DOM element id here, findable only by the
        // generic resolver.
        let html_path = tmp.path().join("ordinary.html");
        std::fs::write(
            &html_path,
            "<html><body><section id=\"t-aaaaaaaaaaaa\"><p>ordinary doc, not a capture</p></section></body></html>",
        )
        .unwrap();
        let id = crate::ids::ArtifactId::from_path("ordinary.html");

        let mut review = ReviewFile::empty_skeleton(&kb, id.as_str(), "ordinary");
        review.comments.push(Comment {
            id: "c_ord".into(),
            status: CommentStatus::Open,
            file: id.as_str().to_string(),
            file_label: "main".into(),
            anchor: Anchor::Section {
                id: "t-aaaaaaaaaaaa".into(),
                tag: Some("section".into()),
                snippet: None,
            },
            author: Author::You,
            body: "ordinary anchor".into(),
            created_at: Utc::now(),
            edited_at: None,
            replies: vec![],
            choices: vec![],
            attachments: vec![],
            user: None,
        });
        let review_path = review_dir.join(format!("{}.json", id.as_str()));
        crate::review::save_atomic(&review_path, &review, None).unwrap();

        let indexer_rx = bus.subscribe();
        let _indexer = tokio::spawn(run(
            kb.clone(),
            slug.clone(),
            _storage.clone(),
            bus.clone(),
            quarantine.clone(),
            indexer_rx,
            None,
            Some(review_dir.clone()),
            crate::iframe::DEFAULT_HOST_SUFFIX.to_string(),
            crate::vcs::VersionsMode::Off,
            0,
            Vec::new(),
        ));

        let mut rx = bus.subscribe();
        bus.emit(
            "watch.create",
            json!({"kb": kb.as_str(), "path": html_path.to_string_lossy()}),
        );
        let mut saw_stale = false;
        timeout(Duration::from_secs(5), async {
            loop {
                match rx.recv().await {
                    Ok(env) if env.type_ == "comment.anchor_stale" => {
                        saw_stale = true;
                    }
                    Ok(env)
                        if env.type_ == "artifact.indexed"
                            && env.payload["artifact_id"].as_str() == Some(id.as_str()) =>
                    {
                        return;
                    }
                    Ok(_) => continue,
                    Err(_) => return,
                }
            }
        })
        .await
        .expect("artifact.indexed never fired");
        assert!(
            !saw_stale,
            "an ordinary doc's Section anchor must still resolve via the \
             generic DOM resolver (id genuinely present in the DOM)"
        );
    }

    #[tokio::test]
    async fn comment_anchor_resolved_does_not_fire_when_never_stale() {
        // Comment whose anchor binds on the first reindex never went
        // stale; the resolved event must NOT fire (we'd be telling
        // the SPA to clear a badge it never painted).
        use crate::review::{Anchor, Author, Comment, CommentStatus, ReviewFile};
        use chrono::Utc;
        let (bus, _storage, kb, slug, tmp) = setup().await;
        let quarantine = tmp.path().join("quarantine");
        let review_dir = tmp.path().join(".review");
        std::fs::create_dir_all(&review_dir).unwrap();

        let html_path = tmp.path().join("steady.html");
        let html = "<html><body><section id=\"hero\"><p>steady</p></section></body></html>";
        std::fs::write(&html_path, html).unwrap();
        let id = crate::ids::ArtifactId::from_path("steady.html");

        let mut review = ReviewFile::empty_skeleton(&kb, id.as_str(), "steady");
        review.comments.push(Comment {
            id: "c_steady".into(),
            status: CommentStatus::Open,
            file: id.as_str().to_string(),
            file_label: "main".into(),
            anchor: Anchor::Section {
                id: "hero".into(),
                tag: Some("section".into()),
                snippet: None,
            },
            author: Author::You,
            body: "anchor was never stale".into(),
            created_at: Utc::now(),
            edited_at: None,
            replies: vec![],
            choices: vec![],
            attachments: vec![],
            user: None,
        });
        let review_path = review_dir.join(format!("{}.json", id.as_str()));
        crate::review::save_atomic(&review_path, &review, None).unwrap();

        let indexer_rx = bus.subscribe();
        let bus_for_indexer = bus.clone();
        let storage_for_indexer = _storage.clone();
        let kb_for_indexer = kb.clone();
        let slug_for_indexer = slug.clone();
        let quarantine_for_indexer = quarantine.clone();
        let review_for_indexer = Some(review_dir.clone());
        let _indexer = tokio::spawn(async move {
            run(
                kb_for_indexer,
                slug_for_indexer,
                storage_for_indexer,
                bus_for_indexer,
                quarantine_for_indexer,
                indexer_rx,
                None,
                review_for_indexer,
                crate::iframe::DEFAULT_HOST_SUFFIX.to_string(),
                crate::vcs::VersionsMode::Off,
                0,
                Vec::new(),
            )
            .await
        });

        let mut rx = bus.subscribe();
        bus.emit(
            "watch.create",
            json!({"kb": kb.as_str(), "path": html_path.to_string_lossy()}),
        );

        let result = timeout(Duration::from_millis(800), async {
            loop {
                match rx.recv().await {
                    Ok(env) if env.type_ == "comment.anchor_resolved" => return Some(env),
                    Ok(env) if env.type_ == "index.complete" => return None,
                    _ => continue,
                }
            }
        })
        .await
        .ok()
        .flatten();
        assert!(
            result.is_none(),
            "anchor_resolved must NOT fire when no prior stale was seen"
        );
    }

    #[tokio::test]
    async fn anchor_stale_set_survives_indexer_restart() {
        // v0.7 P1: a comment that went stale under one indexer process
        // must still be able to emit `comment.anchor_resolved` after a
        // restart. Indexer #1 records the stale anchor + persists the
        // sidecar; indexer #2 (fresh in-process state, same review_dir)
        // rehydrates from the sidecar and fires anchor_resolved on the
        // resolve pass. Without the sidecar this would be silent.
        use crate::review::{Anchor, Author, Comment, CommentStatus, ReviewFile};
        use chrono::Utc;
        let (bus, storage, kb, slug, tmp) = setup().await;
        let quarantine = tmp.path().join("quarantine");
        let review_dir = tmp.path().join(".review");
        std::fs::create_dir_all(&review_dir).unwrap();

        let html_path = tmp.path().join("restart.html");
        let html_missing = "<html><body><p>no section</p></body></html>";
        let html_present =
            "<html><body><section id=\"appendix\"><p>back!</p></section></body></html>";
        // Artifact id is the source-relative path, so it's the SAME
        // across both HTML states — one review file covers both passes.
        let id = crate::ids::ArtifactId::from_path("restart.html");

        // Generous hang-guards, not perf assertions. This test spawns TWO
        // indexer tasks in sequence (the restart), so under heavy CI
        // contention its sub-second event + sidecar operations can take
        // several seconds — the old 5 s budget flaked there (a tokio Elapsed
        // timeout under load, green in isolation). 30 s never trips for a
        // working pipeline but still bounds a genuine hang.
        const WAIT: Duration = Duration::from_secs(30);

        let mut review = ReviewFile::empty_skeleton(&kb, id.as_str(), "restart");
        review.comments.push(Comment {
            id: "c_restart".into(),
            status: CommentStatus::Open,
            file: id.as_str().to_string(),
            file_label: "main".into(),
            anchor: Anchor::Section {
                id: "appendix".into(),
                tag: Some("section".into()),
                snippet: None,
            },
            author: Author::You,
            body: "stale set must survive a restart".into(),
            created_at: Utc::now(),
            edited_at: None,
            replies: vec![],
            choices: vec![],
            attachments: vec![],
            user: None,
        });
        crate::review::save_atomic(
            &review_dir.join(format!("{}.json", id.as_str())),
            &review,
            None,
        )
        .unwrap();

        // --- Indexer #1: induce the stale anchor, then abort it. ---
        let indexer1 = tokio::spawn(run(
            kb.clone(),
            slug.clone(),
            storage.clone(),
            bus.clone(),
            quarantine.clone(),
            bus.subscribe(),
            None,
            Some(review_dir.clone()),
            crate::iframe::DEFAULT_HOST_SUFFIX.to_string(),
            crate::vcs::VersionsMode::Off,
            0,
            Vec::new(),
        ));
        let mut rx = bus.subscribe();
        std::fs::write(&html_path, html_missing).unwrap();
        bus.emit(
            "watch.create",
            json!({"kb": kb.as_str(), "path": html_path.to_string_lossy()}),
        );
        timeout(WAIT, async {
            loop {
                match rx.recv().await {
                    Ok(env) if env.type_ == "comment.anchor_stale" => return env,
                    _ => continue,
                }
            }
        })
        .await
        .expect("anchor_stale never fired under indexer #1");

        // The sidecar write happens synchronously right after the emit,
        // but the broadcast recv above can observe the event a hair
        // earlier — poll the sidecar until the id lands.
        let sidecar = crate::anchors::sidecar_path(&review_dir);
        let deadline = Instant::now() + WAIT;
        // v0.7.1 P2 — the sidecar is now keyed on `(artifact_id,
        // comment_id)`; check for the comment id on any artifact.
        while !crate::anchors::load(&sidecar)
            .keys()
            .any(|(_, cid)| cid == "c_restart")
        {
            assert!(
                Instant::now() < deadline,
                "stale comment id never persisted to the sidecar"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        indexer1.abort();

        // --- Indexer #2: fresh in-process state, same review_dir. ---
        let indexer2 = tokio::spawn(run(
            kb.clone(),
            slug.clone(),
            storage.clone(),
            bus.clone(),
            quarantine.clone(),
            bus.subscribe(),
            None,
            Some(review_dir.clone()),
            crate::iframe::DEFAULT_HOST_SUFFIX.to_string(),
            crate::vcs::VersionsMode::Off,
            0,
            Vec::new(),
        ));
        let mut rx2 = bus.subscribe();
        std::fs::write(&html_path, html_present).unwrap();
        bus.emit(
            "watch.modify",
            json!({"kb": kb.as_str(), "path": html_path.to_string_lossy()}),
        );
        let resolved = timeout(WAIT, async {
            loop {
                match rx2.recv().await {
                    Ok(env) if env.type_ == "comment.anchor_resolved" => return env,
                    _ => continue,
                }
            }
        })
        .await
        .expect("anchor_resolved never fired after restart — sidecar not rehydrated");
        assert_eq!(resolved.payload["comment_id"].as_str(), Some("c_restart"));
        indexer2.abort();

        // After resolution the sidecar must no longer list the comment.
        let deadline = Instant::now() + WAIT;
        while crate::anchors::load(&sidecar)
            .keys()
            .any(|(_, cid)| cid == "c_restart")
        {
            assert!(
                Instant::now() < deadline,
                "resolved comment id never cleared from the sidecar"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// A relative `<a href="../sibling.html">` from one indexed file to
    /// another should land as an outbound edge in sqlite, keyed by the
    /// content-hash id of the target — even though the link itself
    /// carries no artifact id.
    #[tokio::test]
    async fn cross_artifact_relative_link_records_edge() {
        let (bus, storage, kb, slug, tmp) = setup().await;
        let quarantine = tmp.path().join("quarantine");

        let indexer_rx = bus.subscribe();
        let _indexer = tokio::spawn({
            let bus = bus.clone();
            let storage = storage.clone();
            let kb = kb.clone();
            let slug = slug.clone();
            let quarantine = quarantine.clone();
            async move {
                run(
                    kb,
                    slug,
                    storage,
                    bus,
                    quarantine,
                    indexer_rx,
                    None,
                    None,
                    crate::iframe::DEFAULT_HOST_SUFFIX.to_string(),
                    crate::vcs::VersionsMode::Off,
                    0,
                    Vec::new(),
                )
                .await
            }
        });

        // Target file lives at <root>/incidents/checks/check.html;
        // source file lives at <root>/changelog/daily/log.html and
        // links to it via `../../incidents/checks/check.html`.
        let target_dir = tmp.path().join("incidents").join("checks");
        std::fs::create_dir_all(&target_dir).unwrap();
        let target_path = target_dir.join("check.html");
        std::fs::write(
            &target_path,
            "<html><title>Check</title><body>target body</body></html>",
        )
        .unwrap();

        let source_dir = tmp.path().join("changelog").join("daily");
        std::fs::create_dir_all(&source_dir).unwrap();
        let source_path = source_dir.join("log.html");
        std::fs::write(
            &source_path,
            r#"<html><title>Log</title><body><a href="../../incidents/checks/check.html">go</a></body></html>"#,
        )
        .unwrap();

        let mut rx = bus.subscribe();

        // Index target first so the cross-link can find it.
        bus.emit(
            "watch.create",
            json!({"kb": kb.as_str(), "path": target_path.to_string_lossy()}),
        );
        timeout(Duration::from_secs(5), async {
            loop {
                match rx.recv().await {
                    Ok(env) if env.type_ == "artifact.indexed" => return env,
                    _ => continue,
                }
            }
        })
        .await
        .expect("target indexed timeout");

        bus.emit(
            "watch.create",
            json!({"kb": kb.as_str(), "path": source_path.to_string_lossy()}),
        );
        let env = timeout(Duration::from_secs(5), async {
            loop {
                match rx.recv().await {
                    Ok(env)
                        if env.type_ == "artifact.indexed"
                            && env.payload["path"].as_str()
                                == Some(source_path.to_string_lossy().as_ref()) =>
                    {
                        return env
                    }
                    _ => continue,
                }
            }
        })
        .await
        .expect("source indexed timeout");

        let source_id = env.payload["artifact_id"]
            .as_str()
            .expect("artifact_id in payload")
            .to_string();

        // Pull the target's id by exact-path lookup (the indexer's own
        // canonical absolute path matches what we wrote into lance).
        let target_canon = target_path.canonicalize().unwrap();
        let target_summary = storage
            .get_by_source_path(target_canon.to_string_lossy().to_string())
            .await
            .unwrap()
            .expect("target row exists");

        // The edge must exist outbound from source → target.
        let edges = storage.edges_from(source_id.clone(), 1).await.unwrap();
        assert!(
            edges.iter().any(|e| e.to_id == target_summary.id),
            "expected outbound edge from {source_id} to {target}, got {edges:?}",
            target = target_summary.id
        );
    }

    /// L3 — re-seed immunity: once a memory artifact has been seeded
    /// into V0010 from its HTML metas, a subsequent Modified event
    /// (even one that adds/changes metas) must NOT re-import them.
    /// The V0011 tombstone is the source of truth; UI-driven mutations
    /// are durable across file rewrites.
    #[tokio::test]
    async fn memory_link_seed_is_one_shot_and_re_index_is_a_noop() {
        let (bus, storage, kb, slug, tmp) = setup().await;
        let quarantine = tmp.path().join("quarantine");

        let indexer_rx = bus.subscribe();
        let _indexer = tokio::spawn({
            let bus = bus.clone();
            let storage = storage.clone();
            let kb = kb.clone();
            let slug = slug.clone();
            let quarantine = quarantine.clone();
            async move {
                run(
                    kb,
                    slug,
                    storage,
                    bus,
                    quarantine,
                    indexer_rx,
                    None,
                    None,
                    crate::iframe::DEFAULT_HOST_SUFFIX.to_string(),
                    crate::vcs::VersionsMode::Off,
                    0,
                    Vec::new(),
                )
                .await
            }
        });

        // Memory artifact with two explicit links + non-global. After
        // the first index, V0010 must contain exactly {kb-a, kb-b};
        // the V0011 tombstone must be set.
        let html_path = tmp.path().join("mem.html");
        let html = r#"<html><head>
            <meta name="kb-category" content="memory-user">
            <meta name="kb-linked-kbs" content="kb-a, kb-b">
            <title>Seed me</title>
        </head><body><p>x</p></body></html>"#;
        std::fs::write(&html_path, html).unwrap();
        let mut rx = bus.subscribe();
        bus.emit(
            "watch.create",
            json!({"kb": kb.as_str(), "path": html_path.to_string_lossy()}),
        );
        timeout(Duration::from_secs(5), async {
            loop {
                match rx.recv().await {
                    Ok(env) if env.type_ == "artifact.indexed" => return env,
                    _ => continue,
                }
            }
        })
        .await
        .expect("first index never landed");

        let id = ArtifactId::from_path("mem.html").as_str().to_string();
        assert!(storage.memory_links_seeded_has(id.clone()).await.unwrap());
        let mut links = storage.memory_links_for(id.clone()).await.unwrap();
        links.sort();
        assert_eq!(links, vec!["kb-a", "kb-b"]);

        // Simulate the user clearing every link via the UI (the
        // mutation routes do this directly through the actor).
        storage
            .memory_links_replace(id.clone(), Vec::new(), false, unix_now())
            .await
            .unwrap();
        assert!(storage
            .memory_links_for(id.clone())
            .await
            .unwrap()
            .is_empty());

        // Rewrite the file with DIFFERENT metas (would-be re-seed if
        // the tombstone didn't win). Re-index path must NOT touch
        // V0010.
        let html2 = r#"<html><head>
            <meta name="kb-category" content="memory-user">
            <meta name="kb-global" content="true">
            <meta name="kb-linked-kbs" content="kb-x,kb-y,kb-z">
            <title>Seed me v2</title>
        </head><body><p>different bytes</p></body></html>"#;
        std::fs::write(&html_path, html2).unwrap();
        bus.emit(
            "watch.modify",
            json!({"kb": kb.as_str(), "path": html_path.to_string_lossy()}),
        );
        timeout(Duration::from_secs(5), async {
            loop {
                match rx.recv().await {
                    Ok(env)
                        if env.type_ == "artifact.indexed"
                            && env.payload["change_kind"].as_str() == Some("modified") =>
                    {
                        return env
                    }
                    _ => continue,
                }
            }
        })
        .await
        .expect("modify-event index never landed");

        // V0010 must still be empty — the metas did NOT re-seed.
        assert!(
            storage
                .memory_links_for(id.clone())
                .await
                .unwrap()
                .is_empty(),
            "re-index re-seeded; tombstone failed to hold"
        );
    }

    /// L3 (revised) — `memory-session` artifacts (verbatim conversation
    /// transcripts dropped by `plugins/kb-memory/hooks/kb-capture.sh`) are deliberately
    /// excluded from the seed gate. They're per-session captures, not
    /// user-level memories; surfacing them via the for_kb-filtered
    /// /memory views and PreviewInspector Related-memories rails would
    /// be noise.
    #[tokio::test]
    async fn memory_session_artifacts_are_excluded_from_link_seeding() {
        let (bus, storage, kb, slug, tmp) = setup().await;
        let quarantine = tmp.path().join("quarantine");

        let indexer_rx = bus.subscribe();
        let _indexer = tokio::spawn({
            let bus = bus.clone();
            let storage = storage.clone();
            let kb = kb.clone();
            let slug = slug.clone();
            let quarantine = quarantine.clone();
            async move {
                run(
                    kb,
                    slug,
                    storage,
                    bus,
                    quarantine,
                    indexer_rx,
                    None,
                    None,
                    crate::iframe::DEFAULT_HOST_SUFFIX.to_string(),
                    crate::vcs::VersionsMode::Off,
                    0,
                    Vec::new(),
                )
                .await
            }
        });

        // A session-transcript artifact stamped with kb-global=true +
        // explicit linked_kbs — the seed gate must skip it anyway
        // because category == "memory-session".
        let html_path = tmp.path().join("sess.html");
        let html = r#"<html><head>
            <meta name="kb-category" content="memory-session">
            <meta name="kb-global" content="true">
            <meta name="kb-linked-kbs" content="kb-a,kb-b">
            <meta name="kb-session" content="sess-xyz">
            <title>session transcript</title>
        </head><body><pre>{}</pre></body></html>"#;
        std::fs::write(&html_path, html).unwrap();
        let mut rx = bus.subscribe();
        bus.emit(
            "watch.create",
            json!({"kb": kb.as_str(), "path": html_path.to_string_lossy()}),
        );
        timeout(Duration::from_secs(5), async {
            loop {
                match rx.recv().await {
                    Ok(env) if env.type_ == "artifact.indexed" => return env,
                    _ => continue,
                }
            }
        })
        .await
        .expect("index of session transcript never landed");

        let id = ArtifactId::from_path("sess.html").as_str().to_string();
        // Neither the link rows nor the seeded tombstone should exist —
        // the gate skipped this artifact entirely.
        assert!(
            storage
                .memory_links_for(id.clone())
                .await
                .unwrap()
                .is_empty(),
            "memory-session must NOT seed link rows"
        );
        assert!(
            !storage.memory_links_seeded_has(id).await.unwrap(),
            "memory-session must NOT mark the seeded tombstone"
        );
    }

    /// W0.6 amendment (2026-07-22) — the Arrow-overflow fix, exercised
    /// through the REAL `prepare_doc` pipeline (not just the pure
    /// `sessions::truncate_code_field` unit tests): a synthetic session
    /// whose `<pre>` transcript is far larger than
    /// `sessions::SESSION_CODE_FIELD_CAP_BYTES` must come out with
    /// `doc.code` capped, not flowing through uncapped as it did in the
    /// W0.6 commit that first shipped `fields.code` retention. Also proves
    /// round-trip safety (invariant #27): `code` is an index-time-only
    /// derived field — `prepare_doc` never writes back to the source file,
    /// so the on-disk artifact bytes are byte-identical before and after,
    /// regardless of how aggressively `code` gets truncated for the index.
    #[tokio::test]
    async fn memory_session_code_field_is_capped_and_source_file_is_untouched() {
        let (bus, storage, kb, slug, tmp) = setup().await;
        let quarantine = tmp.path().join("quarantine");
        let metrics = crate::metrics::PipelineMetrics::disabled();
        let extensions = crate::extmap::ExtensionMap::default();
        let indexed_hashes: DedupCache = Arc::new(Mutex::new(HashMap::new()));

        // A pathologically large main transcript — far over
        // SESSION_CODE_FIELD_CAP_BYTES (32 KiB) — modelling the
        // uncapped-main-transcript bug this fix closes. The first JSONL
        // record is a real typed user message so `session_digest` is
        // non-empty (the memory-session branch only runs the `code` cap
        // once it has a digest to substitute `body` with — see
        // `indexer::prepare_doc`); the second is padding to blow well past
        // the cap.
        let first_prompt = r#"{"type":"user","message":{"role":"user","content":"fix the huge bug"},"promptSource":"typed"}"#;
        let huge_line = "x".repeat(200_000);
        let html = format!(
            r#"<html><head><meta name="kb-category" content="memory-session"></head>
            <body><pre>{first_prompt}
{{"line":"{huge_line}"}}</pre></body></html>"#
        );
        let path = tmp.path().join("huge-session.html");
        std::fs::write(&path, &html).unwrap();
        let original_bytes = std::fs::read(&path).unwrap();

        let run_id = storage.begin_run(slug.clone(), unix_now()).await.unwrap();
        let prepared = prepare_doc(
            &kb,
            &slug,
            tmp.path(),
            &storage,
            &bus,
            &quarantine,
            &path,
            ChangeKind::Created,
            false,
            None,
            &run_id,
            indexed_hashes,
            &metrics,
            false,
            &extensions,
            &crate::exclusions::IngestGate::default(),
        )
        .await
        .expect("prepare_doc must succeed")
        .expect("a fresh file must not hit the dedup gate");

        assert!(
            prepared.doc.code.len() <= crate::sessions::SESSION_CODE_FIELD_CAP_BYTES,
            "code field must be capped: got {} bytes",
            prepared.doc.code.len()
        );
        assert!(
            prepared.doc.code.contains("[kb-code: truncated"),
            "an over-cap transcript must leave the truncation marker in `code`"
        );

        // Round-trip safety: prepare_doc must never write back to the
        // source file — the on-disk artifact is byte-identical before and
        // after, however aggressively `code` got capped for the index.
        let after_bytes = std::fs::read(&path).unwrap();
        assert_eq!(
            original_bytes, after_bytes,
            "capping the index-time `code` field must not touch the on-disk artifact"
        );
    }

    /// CT-A1 (U3 parse-back) — a highlight-born memory's provenance metas
    /// (`kb-author`/`kb-source-kb`/`kb-source-artifact`/`kb-source-anchor`)
    /// survive the FULL indexer path (`prepare_doc`, the same fn every
    /// `watch.create`/`watch.modify` event drives) and land on the `Doc`
    /// the storage actor upserts — then read back through the widened
    /// lance projection, mirroring `memory_metas_read_back_through_search_
    /// projection` in `storage/lance.rs`.
    #[tokio::test]
    async fn u3_provenance_survives_the_full_indexer_path() {
        let (bus, storage, kb, slug, tmp) = setup().await;
        let quarantine = tmp.path().join("quarantine");
        let metrics = crate::metrics::PipelineMetrics::disabled();
        let extensions = crate::extmap::ExtensionMap::default();
        let indexed_hashes: DedupCache = Arc::new(Mutex::new(HashMap::new()));

        let provenance = crate::memory::MemoryProvenance {
            author: Some(crate::review::Author::You),
            source_kb: Some("kb-docs".into()),
            source_artifact: Some("a1b2c3d4e5f6".into()),
            source_anchor: Some(crate::review::Anchor::Section {
                id: "intro".into(),
                tag: None,
                snippet: None,
            }),
            source: None,
        };
        let html = crate::memory::render_artifact(
            "Highlighted claim",
            &crate::memory::text_to_body_html("the selection, verbatim"),
            "memory-user",
            &[],
            None,
            None,
            None,
            None,
            false,
            &[],
            None,
            None,
            Some(&provenance),
            None,
            None,
        );
        let path = tmp.path().join("highlight.html");
        std::fs::write(&path, &html).unwrap();

        let run_id = storage.begin_run(slug.clone(), unix_now()).await.unwrap();
        let prepared = prepare_doc(
            &kb,
            &slug,
            tmp.path(),
            &storage,
            &bus,
            &quarantine,
            &path,
            ChangeKind::Created,
            false,
            None,
            &run_id,
            indexed_hashes,
            &metrics,
            false,
            &extensions,
            &crate::exclusions::IngestGate::default(),
        )
        .await
        .expect("prepare_doc must succeed")
        .expect("a fresh file must not hit the dedup gate");

        assert_eq!(prepared.doc.kb_author.as_deref(), Some("you"));
        assert_eq!(prepared.doc.kb_source_kb.as_deref(), Some("kb-docs"));
        assert_eq!(
            prepared.doc.kb_source_artifact.as_deref(),
            Some("a1b2c3d4e5f6")
        );
        let anchor_json = prepared
            .doc
            .kb_source_anchor
            .clone()
            .expect("anchor must be parsed back");
        assert_eq!(
            anchor_json,
            crate::lists::anchor_to_json(&crate::review::Anchor::Section {
                id: "intro".into(),
                tag: None,
                snippet: None,
            })
        );

        // Upsert (same as the real pipeline) and read back through the
        // storage layer's widened projection — the actual read surface
        // `/memory/recall` and `/memory/census` query.
        storage
            .upsert_docs(vec![prepared.doc.clone()])
            .await
            .unwrap();
        let row = storage
            .get_by_id(prepared.doc.id.clone())
            .await
            .unwrap()
            .expect("row must be indexed");
        assert_eq!(row.id, prepared.doc.id);
    }

    // --- RL3 — reading-list entry anchor lifecycle -------------------------

    /// Insert one list + one section-anchored entry for `rel_path` through
    /// the storage handle, returning the entry id.
    async fn seed_list_entry(
        storage: &StorageHandle,
        rel_path: &str,
        section_id: &str,
    ) -> (String, String) {
        let list = storage
            .list_create(
                crate::lists::new_list_id(),
                format!("Anchor test {rel_path}"),
                None,
                false,
                unix_now(),
            )
            .await
            .unwrap();
        let anchor = crate::review::Anchor::Section {
            id: section_id.to_string(),
            tag: None,
            snippet: None,
        };
        let entry = storage
            .list_entry_add(
                crate::lists::NewListEntry {
                    id: crate::lists::new_entry_id(),
                    list_id: list.id.clone(),
                    kb: "smoke".into(),
                    artifact_id: crate::ids::ArtifactId::from_path(rel_path)
                        .as_str()
                        .to_string(),
                    anchor_json: Some(crate::lists::anchor_to_json(&anchor)),
                    note: None,
                    words: None,
                    read_override: None,
                },
                crate::lists::PositionSpec::Last,
                "operator".to_string(),
                unix_now(),
            )
            .await
            .unwrap();
        (list.id, entry.id)
    }

    fn spawn_indexer(
        bus: &Arc<EventBus>,
        storage: &StorageHandle,
        kb: &KbName,
        slug: &SourceSlug,
        quarantine: &std::path::Path,
    ) -> tokio::task::JoinHandle<()> {
        let indexer_rx = bus.subscribe();
        let bus_i = bus.clone();
        let storage_i = storage.clone();
        let kb_i = kb.clone();
        let slug_i = slug.clone();
        let quarantine_i = quarantine.to_path_buf();
        tokio::spawn(async move {
            run(
                kb_i,
                slug_i,
                storage_i,
                bus_i,
                quarantine_i,
                indexer_rx,
                None,
                None,
                crate::iframe::DEFAULT_HOST_SUFFIX.to_string(),
                crate::vcs::VersionsMode::Off,
                0,
                Vec::new(),
            )
            .await
        })
    }

    #[tokio::test]
    async fn list_entry_anchor_stale_fires_when_section_disappears() {
        let (bus, storage, kb, slug, tmp) = setup().await;
        let quarantine = tmp.path().join("quarantine");

        let html_path = tmp.path().join("listed.html");
        std::fs::write(
            &html_path,
            "<html><body><h2 id=\"vanishing-id\">Held</h2><p>one two three</p></body></html>",
        )
        .unwrap();
        let (list_id, entry_id) = seed_list_entry(&storage, "listed.html", "vanishing-id").await;

        // First index: the anchor binds → no stale event, words refreshed.
        let _indexer = spawn_indexer(&bus, &storage, &kb, &slug, &quarantine);
        let mut rx = bus.subscribe();
        bus.emit(
            "watch.create",
            json!({"kb": kb.as_str(), "path": html_path.to_string_lossy()}),
        );
        wait_for_indexed(&mut rx, ArtifactId::from_path("listed.html").as_str()).await;
        let rows = storage
            .list_entries_for_artifact(ArtifactId::from_path("listed.html").as_str().to_string())
            .await
            .unwrap();
        assert!(!rows[0].anchor_stale, "binding anchor must not be stale");
        assert!(
            rows[0].words.is_some_and(|w| w > 0),
            "hook refreshes the section words estimate: {rows:?}"
        );

        // Regen WITHOUT the section id → 0→1 transition fires the event.
        let mut rx = bus.subscribe();
        std::fs::write(
            &html_path,
            "<html><body><h2 id=\"new-id\">Rewritten</h2><p>different</p></body></html>",
        )
        .unwrap();
        bus.emit(
            "watch.modify",
            json!({"kb": kb.as_str(), "path": html_path.to_string_lossy()}),
        );
        let stale = timeout(Duration::from_secs(5), async {
            loop {
                match rx.recv().await {
                    Ok(env) if env.type_ == "list.entry.anchor_stale" => return env,
                    _ => continue,
                }
            }
        })
        .await
        .expect("list.entry.anchor_stale never fired");
        assert_eq!(stale.payload["kb"].as_str(), Some(kb.as_str()));
        assert_eq!(stale.payload["list_id"].as_str(), Some(list_id.as_str()));
        assert_eq!(stale.payload["entry_id"].as_str(), Some(entry_id.as_str()));
        assert_eq!(stale.payload["anchor_kind"].as_str(), Some("section"));
        assert_eq!(
            stale.payload["source_relative"].as_str(),
            Some("listed.html")
        );
        // Persisted on the row (restart-safe by construction).
        let rows = storage
            .list_entries_for_artifact(ArtifactId::from_path("listed.html").as_str().to_string())
            .await
            .unwrap();
        assert!(rows[0].anchor_stale);
    }

    #[tokio::test]
    async fn list_entry_anchor_resolved_fires_on_restore_via_fresh_indexer() {
        let (bus, storage, kb, slug, tmp) = setup().await;
        let quarantine = tmp.path().join("quarantine");

        let html_path = tmp.path().join("restore.html");
        // Starts WITHOUT the section → first index marks the entry stale.
        std::fs::write(
            &html_path,
            "<html><body><h2 id=\"other\">Other</h2></body></html>",
        )
        .unwrap();
        let (_list_id, entry_id) = seed_list_entry(&storage, "restore.html", "wanted").await;

        let indexer_a = spawn_indexer(&bus, &storage, &kb, &slug, &quarantine);
        let mut rx = bus.subscribe();
        bus.emit(
            "watch.create",
            json!({"kb": kb.as_str(), "path": html_path.to_string_lossy()}),
        );
        timeout(Duration::from_secs(5), async {
            loop {
                match rx.recv().await {
                    Ok(env) if env.type_ == "list.entry.anchor_stale" => return,
                    _ => continue,
                }
            }
        })
        .await
        .expect("stale never fired on first index");

        // Kill indexer A; a FRESH indexer (empty in-process state) must
        // still detect the 1→0 transition — the base state is the ROW.
        indexer_a.abort();
        let _indexer_b = spawn_indexer(&bus, &storage, &kb, &slug, &quarantine);
        let mut rx = bus.subscribe();
        std::fs::write(
            &html_path,
            "<html><body><h2 id=\"wanted\">Wanted</h2><p>four five six seven</p></body></html>",
        )
        .unwrap();
        bus.emit(
            "watch.modify",
            json!({"kb": kb.as_str(), "path": html_path.to_string_lossy()}),
        );
        let resolved = timeout(Duration::from_secs(5), async {
            loop {
                match rx.recv().await {
                    Ok(env) if env.type_ == "list.entry.anchor_resolved" => return env,
                    _ => continue,
                }
            }
        })
        .await
        .expect("list.entry.anchor_resolved never fired");
        assert_eq!(
            resolved.payload["entry_id"].as_str(),
            Some(entry_id.as_str())
        );
        let rows = storage
            .list_entries_for_artifact(ArtifactId::from_path("restore.html").as_str().to_string())
            .await
            .unwrap();
        assert!(!rows[0].anchor_stale, "row state cleared after resolve");
        assert!(
            rows[0].words.is_some_and(|w| w > 0),
            "resolve refreshes words: {rows:?}"
        );
    }
}
