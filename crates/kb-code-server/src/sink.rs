//! W1.6 — the real [`mirror::MirrorSink`]: turns live-watcher observations
//! into store mutations via `ingest`, off the watcher's dedicated
//! `std::thread` (`mirror::MirrorWatcher::start`'s doc: that thread must
//! never block on parsing).
//!
//! # Shape
//!
//! [`IndexSink`] is a thin, synchronous producer — every `MirrorSink` method
//! is a `blocking_send` onto a bounded `tokio::mpsc` channel. `blocking_send`
//! is legal here for the exact reason
//! `kb_core::indexer::IngestSink::blocking_send` documents its own use: the
//! caller (`mirror`'s drain thread) is a plain OS thread, never a tokio
//! runtime worker, so blocking it to apply back-pressure is safe and
//! intentional — it is NOT called from async code anywhere in this crate.
//! [`spawn`] starts the async worker task that actually reads bytes, hashes
//! them, and writes the store; that task is where all the real I/O and
//! tree-sitter parsing happens, keeping the watcher's drain thread free to
//! keep draining `notify` events (including OTHER repos' git-diff/dirty-check
//! subprocess work) while a big file is being parsed.
//!
//! # Fairness (V77-P2, the E6 finding)
//!
//! Before this unit, `lib.rs::bind_and_spawn` ran a SECOND, unqueued walker:
//! a direct `spawn_blocking` task called straight into `ingest`, racing this
//! module's own worker for `Store`'s single connection with no scheduling
//! relationship between the two at all — a live edit made during that boot
//! walk could sit behind the ENTIRE walk (measured: ~9 minutes on a large
//! mirror) with `GET /api/repos` reporting nothing to explain why. There is
//! now exactly ONE walker (this worker) and exactly ONE queue, split into a
//! FAST lane ([`FastMsg`]: `Upsert`/`Remove`/`HeadMoved` — the live-edit
//! path, still bounded by [`QUEUE_CAPACITY`]) and a SLOW lane ([`SlowMsg`]:
//! `FullReconcile` and the boot walk's own `BootWalk`, bounded by
//! [`SLOW_QUEUE_CAPACITY`]). `bind_and_spawn`'s former direct call is now
//! just another SLOW-lane producer ([`IndexSink::enqueue_boot_walk`]) —
//! see that method's doc.
//!
//! A slow-lane message is never processed in one unbroken pass: it is
//! chunked into bounded sub-batches ([`RECONCILE_CHUNK_SIZE`] paths/files
//! per chunk, both for a `FullReconcile`'s changed/removed set and for the
//! boot walk's flattened file list — see [`ReconcileJob`]/[`BootJob`]).
//! [`worker`]'s scheduling loop always drains up to [`FAST_BURST_LIMIT`]
//! fast-lane messages BEFORE giving the active slow job its next chunk, on
//! every iteration — a FAIR interleave, not a strict priority: a live-edit
//! storm gets serviced promptly (bounded fast-message latency, regardless
//! of how large the queued slow job is) but can never livelock the slow
//! job forever, because the loop always attempts exactly one slow chunk
//! per burst regardless of how much fast-lane traffic remains. This is
//! queue-level fairness only — `Store`'s own single connection/mutex is
//! still the final serialization point every message (fast or slow) goes
//! through, unchanged.
//!
//! Ordering is preserved across the two lanes: `mirror::MirrorSink`'s
//! contract that a `head_moved` (fast) is always immediately followed by
//! its own `full_reconcile` (slow) call, from the SAME single producer
//! thread, means a `head_moved` is always already sitting in (or already
//! drained from) the fast lane by the time the worker picks up the paired
//! `full_reconcile` — and the scheduling loop always attempts a fast drain
//! immediately before processing ANY slow chunk (first or continuation),
//! so `repo.head_moved`'s bus event can never be observed after the first
//! `mirror.updated` chunk event for the same operation.
//!
//! [`RepoActivity`] is the resulting honesty signal: an in-memory,
//! per-repo `pending` counter incremented when a slow-lane message is
//! ENQUEUED and decremented when the worker finishes EVERY chunk of it —
//! `catching_up` is simply `pending > 0`. Scoped to the slow lane only (an
//! ordinary live edit completing in ~2.7s is not "catching up" on
//! anything); read by `routes::repos` for `GET /api/repos`'s
//! `catching_up`/`settled_at` fields, mirroring the unpersisted `rekey`
//! lifecycle flag's own "an honesty flag, never a capability" posture
//! (`routes.rs`). [`test_chunk_delay`] is an off-by-default test hook
//! (`KB_CODE_TEST_CHUNK_DELAY_MS`) letting the `boot_e2e` integration test
//! for this signal force a host-speed-independent walk duration — see that
//! function's own doc.
//!
//! # Backpressure choice
//!
//! [`QUEUE_CAPACITY`] bounds how far the watcher can get ahead of the
//! worker. When full, `blocking_send` blocks the watcher's drain thread —
//! deliberately NOT a `try_send`-and-drop: a dropped `upsert_path` is merely
//! stale (the next edit, or the watcher's own reconcile, re-reports it), but
//! a dropped `remove_path` would leave a `files` row for a path that's
//! genuinely gone with nothing left to correct it (reconcile's `removed` set
//! only re-derives from a fresh committed diff or a currently-`held` dirty
//! check — a path that was never held and whose single `remove_path` event
//! got dropped is invisible to every future reconcile). Mirrors
//! `kb_core::indexer::IngestSink`'s own documented choice for the same
//! reason. Sized well above what a single debounced flush realistically
//! produces (`notify-debouncer-full` already coalesces a burst within its
//! own debounce window; a `full_reconcile` batches an entire changed/removed
//! set into ONE message, not one per path) — a persistently-full queue
//! means the worker itself is the bottleneck (e.g. parsing a genuinely huge
//! churn), not a burst size this margin should have absorbed.
//! [`SLOW_QUEUE_CAPACITY`] is far smaller: a slow-lane message represents a
//! WHOLE repo's worth of work, not one path, so even a large configured
//! fleet plus a burst of concurrent gate-exit reconciles is nowhere near
//! this many outstanding at once — the same never-drop `blocking_send`/
//! `.send().await` choice applies regardless.
//!
//! # Events
//!
//! Every store mutation the worker performs also emits onto the shared
//! `kb_core::events::EventBus` (`GET /api/events`, `router.rs`): `"mirror.
//! updated" {repo, paths}` after an upsert/remove/reconcile CHUNK/boot-walk
//! CHUNK actually touches the store (V77-P2: a slow job now emits one such
//! event per completed chunk that touched at least one path, rather than a
//! single event at the very end — a live progress signal for any SSE
//! listener, and never claimed for a path the V77-P1 fingerprint fast path
//! merely skipped), and `"repo.head_moved" {repo, old, new}` for every
//! [`mirror::MirrorSink::head_moved`] call (mirrors `mirror::MirrorSink`'s
//! own contract: always followed by exactly one `full_reconcile`, so a
//! `repo.head_moved` frame is always followed by at least one `mirror.
//! updated` frame for the same operation, in that order, since both are
//! emitted from the SAME single-consumer worker loop — see "Fairness"
//! above).
//!
//! # Observability
//!
//! [`worker`] processes one fast message or one slow chunk at a time (see
//! "Fairness" above), so the only way to tell "keeping up" from "falling
//! behind" from the outside is to watch it: every [`SUMMARY_EVERY_MESSAGES`]-th
//! unit processed (or every [`SUMMARY_EVERY`] of wall time, whichever comes
//! first — so a quiet queue still gets a heartbeat) a `tracing::info!`
//! "progress summary" line reports the window's fast-queue-length
//! high-water mark (sampled via `Receiver::len()` right after each
//! dequeue, i.e. the backlog still waiting behind the message just taken)
//! against [`QUEUE_CAPACITY`], plus the max and average `spawn_blocking`
//! duration for units processed in that window. This is DETECTION
//! only — no throttling, no alerting wired from it yet — enough for an
//! operator or `kb-code fleet`-equivalent log scrape to notice a
//! bottleneck onset before the queue is actually full. [`ProgressWindow`]
//! is a plain struct so its arithmetic is unit-testable without a running
//! channel. `GET /api/repos`'s `catching_up`/`settled_at` (see "Fairness"
//! above) is the PER-REPO, always-on complement to this daemon-wide,
//! log-only signal.
//!
//! # Shutdown contract — the dropped `JoinHandle` is a decision, not an oversight
//!
//! [`spawn`]'s `JoinHandle` is discarded by every real caller (`lib.rs::
//! bind_and_spawn`) — the worker task runs detached, and whatever messages
//! are still queued (or mid-`spawn_blocking`) at process shutdown are simply
//! lost with it. This is intentionally NOT a graceful-drain-on-shutdown
//! design. It doesn't need to be: ADR-3's boot sequence (`lib.rs::
//! bind_and_spawn`, see that module's doc) already re-walks every
//! configured repo's `HEAD` tree on EVERY boot — once via this module's own
//! boot-walk slow-lane job, and again, redundantly but cheaply (ADR-2's
//! blob-hash cache skips re-parsing unchanged content), via the live
//! watcher's own startup reconcile. Any store mutation a dropped queued
//! message would have made is therefore re-derived from scratch on the very
//! next boot, with no replay log or drain barrier required to get there —
//! the same "distrust file events, reconcile-don't-replay" posture the rest
//! of `mirror` is built on, applied to the sink's own shutdown edge.

use crate::git::GitRepo;
use crate::ingest;
use crate::mirror::{MirrorSink, RepoRef};
use crate::store::{FileRow, Store};
use kb_core::events::EventBus;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TryRecvError;

/// See the module doc's "Backpressure choice" section — the FAST lane.
pub const QUEUE_CAPACITY: usize = 512;

/// See the module doc's "Backpressure choice" section — the SLOW lane.
const SLOW_QUEUE_CAPACITY: usize = 256;

/// V77-P2 (task 1) — fairness bound: the worker drains at most this many
/// fast-lane messages before giving the active slow job its next chunk (see
/// the module doc's "Fairness" section). Large enough that a live-edit
/// burst still feels effectively immediate between chunks; bounded so a
/// SUSTAINED fast-lane storm cannot livelock a slow job forever — every
/// iteration of the scheduling loop attempts exactly one slow chunk after
/// at most this many fast messages, regardless of how much fast-lane
/// traffic remains queued.
const FAST_BURST_LIMIT: usize = 32;

/// V77-P2 (task 1) — slow-lane chunk size: a `FullReconcile`/`BootWalk`
/// job's path list is processed at most this many entries at a time
/// between fast-lane drains. Small enough that a live edit queued behind
/// an in-progress slow job is never stuck for more than "one chunk's worth"
/// of wall time, matching the design note's own number.
const RECONCILE_CHUNK_SIZE: usize = 256;

/// Emit a progress summary at least this often, by message count — see the
/// module doc's "Observability" section. Small enough to surface a
/// bottleneck within a handful of seconds under real traffic; large enough
/// that the summary line itself is never a meaningful fraction of the
/// worker's own work.
const SUMMARY_EVERY_MESSAGES: u64 = 200;

/// ...or after this much wall time since the last summary, whichever comes
/// first — so a lightly-loaded queue (well under `SUMMARY_EVERY_MESSAGES`
/// traffic) still gets a periodic heartbeat instead of going silent.
const SUMMARY_EVERY: Duration = Duration::from_secs(60);

/// Rolling stats for the worker's periodic progress summary (see the module
/// doc). Plain bookkeeping, no I/O of its own, deliberately split out of
/// [`worker`] so its arithmetic is unit-testable without a running channel.
#[derive(Debug)]
struct ProgressWindow {
    started: Instant,
    messages: u64,
    /// Max fast-lane `Receiver::len()` observed at dequeue time this window
    /// — the backlog still waiting behind whatever message was just taken.
    queue_high_water: usize,
    max: Duration,
    total: Duration,
}

impl ProgressWindow {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            messages: 0,
            queue_high_water: 0,
            max: Duration::ZERO,
            total: Duration::ZERO,
        }
    }

    /// Record one processed unit (a fast message or one slow chunk):
    /// `queue_len` is the fast-lane backlog sampled at that moment,
    /// `elapsed` is that unit's own `spawn_blocking` duration.
    fn record(&mut self, queue_len: usize, elapsed: Duration) {
        self.messages += 1;
        self.queue_high_water = self.queue_high_water.max(queue_len);
        self.max = self.max.max(elapsed);
        self.total += elapsed;
    }

    /// `true` once this window has accumulated enough messages or enough
    /// wall time to be worth logging — see [`SUMMARY_EVERY_MESSAGES`]/
    /// [`SUMMARY_EVERY`].
    fn due(&self) -> bool {
        self.messages >= SUMMARY_EVERY_MESSAGES || self.started.elapsed() >= SUMMARY_EVERY
    }

    fn avg_ms(&self) -> f64 {
        if self.messages == 0 {
            0.0
        } else {
            self.total.as_secs_f64() * 1000.0 / self.messages as f64
        }
    }
}

/// The FAST lane (V77-P2) — the live-edit path: `Upsert`/`Remove` from the
/// watcher, `HeadMoved` from a HEAD change. Drained preferentially (see the
/// module doc's "Fairness" section) but never exclusively.
#[derive(Debug)]
enum FastMsg {
    Upsert {
        repo: RepoRef,
        path: PathBuf,
    },
    Remove {
        repo: RepoRef,
        path: PathBuf,
    },
    HeadMoved {
        repo: RepoRef,
        old: Option<gix::ObjectId>,
        new: gix::ObjectId,
    },
}

/// The SLOW lane (V77-P2) — chunked, bounded-throughput work: the
/// watcher's own `full_reconcile` calls, and (new in V77-P2)
/// [`IndexSink::enqueue_boot_walk`]'s boot HEAD-tree walk, which used to
/// bypass this queue entirely (see the module doc's "Fairness" section).
#[derive(Debug)]
enum SlowMsg {
    FullReconcile {
        repo: RepoRef,
        changed: Vec<PathBuf>,
        removed: Vec<PathBuf>,
    },
    BootWalk {
        repo_id: i64,
        repo_name: String,
        repo_root: PathBuf,
        occurrences_enabled: bool,
        is_rails: bool,
    },
}

/// V77-P2 (task 2) — per-repo `catching_up`/`settled_at`, the answer to
/// E6's "queued behind a 20-minute walk, or broken?" question. In-memory
/// and per-boot ONLY — never persisted, never a capability, mirroring
/// `routes.rs`'s unpersisted `rekey` lifecycle flag ("an honesty flag,
/// never a capability") and root invariant #10's live-registry posture. A
/// restart re-derives the same state from a fresh boot walk regardless, so
/// there is nothing here worth surviving one.
///
/// `pending` counts OUTSTANDING slow-lane messages for a repo — incremented
/// by the PRODUCER at enqueue time (`IndexSink::full_reconcile`/
/// `enqueue_boot_walk`), decremented by the WORKER once every chunk of that
/// message has been processed. `catching_up` is simply `pending > 0`; the
/// FAST lane (individual live edits) never touches this counter at all — an
/// ordinary edit completing in a couple of seconds is not "catching up" on
/// anything, and counting it would make the signal noisy rather than
/// honest. `settled_at` is the unix timestamp of the last transition from
/// `pending > 0` to `pending == 0`; `None` while `pending > 0` (still
/// walking) and also `None` for a repo this sink has never seen slow-lane
/// work for at all — a valid, honest "nothing to catch up on" rather than a
/// missing-key default standing in for "settled".
#[derive(Debug, Default)]
pub struct RepoActivity {
    inner: parking_lot::Mutex<HashMap<String, ActivityState>>,
}

#[derive(Debug, Clone, Copy, Default)]
struct ActivityState {
    pending: u32,
    settled_at: Option<i64>,
}

impl RepoActivity {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn mark_busy(&self, repo_name: &str) {
        let mut map = self.inner.lock();
        let entry = map.entry(repo_name.to_string()).or_default();
        entry.pending += 1;
        entry.settled_at = None;
    }

    fn mark_drained(&self, repo_name: &str) {
        let mut map = self.inner.lock();
        if let Some(entry) = map.get_mut(repo_name) {
            entry.pending = entry.pending.saturating_sub(1);
            if entry.pending == 0 {
                entry.settled_at = Some(chrono::Utc::now().timestamp());
            }
        }
    }

    /// `(catching_up, settled_at)` for `GET /api/repos` — see this struct's
    /// own doc for the absent-key case.
    pub fn snapshot(&self, repo_name: &str) -> (bool, Option<i64>) {
        let map = self.inner.lock();
        match map.get(repo_name) {
            Some(entry) => (entry.pending > 0, entry.settled_at),
            None => (false, None),
        }
    }
}

/// The real sink — see the module doc. `Clone` is cheap (two `mpsc::Sender`
/// clones plus an `Arc`); `mirror::MirrorWatcher::start` takes `Arc<dyn
/// MirrorSink>`, so in practice only one clone is ever made that way, but
/// `bind_and_spawn` holds its own clone too (see [`IndexSink::
/// enqueue_boot_walk`]), and nothing stops a future caller from holding
/// another to push synthetic observations through the same pipeline.
#[derive(Clone)]
pub struct IndexSink {
    fast_tx: mpsc::Sender<FastMsg>,
    slow_tx: mpsc::Sender<SlowMsg>,
    activity: Arc<RepoActivity>,
}

impl MirrorSink for IndexSink {
    fn upsert_path(&self, repo: &RepoRef, path: &Path) {
        let _ = self.fast_tx.blocking_send(FastMsg::Upsert {
            repo: repo.clone(),
            path: path.to_path_buf(),
        });
    }

    fn remove_path(&self, repo: &RepoRef, path: &Path) {
        let _ = self.fast_tx.blocking_send(FastMsg::Remove {
            repo: repo.clone(),
            path: path.to_path_buf(),
        });
    }

    fn head_moved(&self, repo: &RepoRef, old: Option<gix::ObjectId>, new: gix::ObjectId) {
        let _ = self.fast_tx.blocking_send(FastMsg::HeadMoved {
            repo: repo.clone(),
            old,
            new,
        });
    }

    fn full_reconcile(&self, repo: &RepoRef, changed: Vec<PathBuf>, removed: Vec<PathBuf>) {
        self.activity.mark_busy(&repo.name);
        let _ = self.slow_tx.blocking_send(SlowMsg::FullReconcile {
            repo: repo.clone(),
            changed,
            removed,
        });
    }
}

impl IndexSink {
    /// V77-P2 (task 1) — enqueue the boot HEAD-tree walk for one repo onto
    /// the SAME slow lane `full_reconcile` uses, so `bind_and_spawn`'s boot
    /// task is no longer a second walker racing this worker for `Store`'s
    /// mutex outside any fairness scheme (see the module doc's "Fairness"
    /// section) — it is now just another producer of this one queue.
    /// `async`, not `blocking_send`: the only real caller is
    /// `bind_and_spawn` itself (already an async fn, never the watcher's
    /// `std::thread` drain loop that the `MirrorSink` methods above are
    /// written for), and this call is a cheap channel send, not real work —
    /// the actual walk happens inside [`worker`] once this message is
    /// dequeued.
    pub async fn enqueue_boot_walk(
        &self,
        repo_id: i64,
        repo_name: String,
        repo_root: PathBuf,
        occurrences_enabled: bool,
        is_rails: bool,
    ) {
        self.activity.mark_busy(&repo_name);
        let _ = self
            .slow_tx
            .send(SlowMsg::BootWalk {
                repo_id,
                repo_name,
                repo_root,
                occurrences_enabled,
                is_rails,
            })
            .await;
    }
}

/// Start the worker task and return the [`IndexSink`] handle plus the
/// shared [`RepoActivity`] registry `routes::repos` reads — `bind_and_spawn`
/// passes the sink to `mirror::MirrorWatcher::start` (and keeps its own
/// clone to drive [`IndexSink::enqueue_boot_walk`]) and lets the returned
/// `JoinHandle` run detached (a tokio task keeps running once spawned
/// regardless of whether its handle is held; the task's own exit condition
/// — every `IndexSink` clone dropped, closing both channels — only happens
/// at daemon shutdown, when the `MirrorWatcher` itself is dropped too).
/// `is_rails` (PRR-N3) is a per-repo-NAME map, resolved ONCE by the caller
/// (`lib.rs::bind_and_spawn`, via `frameworks::rails::detect_is_rails` +
/// `config::RailsLensSection::repo_enabled`) — a plain `HashMap` lookup per
/// message here, mirroring `repo_ids`'s own shape, deliberately NOT a
/// re-run of the (filesystem-touching) detection itself. See
/// `ingest::index_file`'s doc for why that detection must never happen
/// per-message: this worker drains ONE file-change event at a time, so a
/// `Gemfile`+`routes.rb` re-check on every keystroke-triggered save in an
/// active Rails repo would defeat the whole point of caching it.
/// `symbol_index` (V77-P2) is the SAME per-boot `SymbolIndex` instance
/// `AppState` shares — the worker warms it for a repo right after that
/// repo's boot-walk job finishes, mirroring what `lib.rs`'s old direct
/// boot task used to do inline (see [`finish_boot_job`]).
pub fn spawn(
    store: Arc<Store>,
    repo_ids: HashMap<String, i64>,
    bus: Arc<EventBus>,
    occurrences: crate::config::OccurrencesSection,
    is_rails: HashMap<String, bool>,
    comment_keywords: crate::comments::KeywordSet,
    symbol_index: Arc<crate::search::SymbolIndex>,
) -> (IndexSink, Arc<RepoActivity>, tokio::task::JoinHandle<()>) {
    let (fast_tx, fast_rx) = mpsc::channel(QUEUE_CAPACITY);
    let (slow_tx, slow_rx) = mpsc::channel(SLOW_QUEUE_CAPACITY);
    let activity = RepoActivity::new();
    let sink = IndexSink {
        fast_tx,
        slow_tx,
        activity: activity.clone(),
    };
    let handle = tokio::spawn(worker(
        fast_rx,
        slow_rx,
        store,
        Arc::new(repo_ids),
        bus,
        Arc::new(occurrences),
        Arc::new(is_rails),
        Arc::new(comment_keywords),
        symbol_index,
        activity.clone(),
    ));
    (sink, activity, handle)
}

/// One repo-relative path/oid pair operated on by a [`ReconcileJob`] chunk
/// — `Changed` re-reads and re-indexes (subject to the fs-mtime fast path,
/// V77-P1), `Removed` deletes.
enum ReconcileOp {
    Changed(PathBuf),
    Removed(PathBuf),
}

/// A `FullReconcile` message being processed a bounded chunk at a time
/// (V77-P2, task 1). `fingerprints` is fetched ONCE when the job starts
/// (`start_reconcile_job`) — the same "one query for the whole repo" shape
/// V77-P1 already established for this path — and shared (via `Arc`, cheap
/// to clone per chunk) across every chunk rather than re-queried.
struct ReconcileJob {
    repo: RepoRef,
    repo_id: i64,
    fingerprints: Arc<HashMap<String, FileRow>>,
    ops: VecDeque<ReconcileOp>,
    occurrences_enabled: bool,
    is_rails: bool,
}

/// A boot HEAD-tree walk (V77-P2, task 1 — formerly `lib.rs`'s direct,
/// unqueued `initial_index_one` call) being processed a bounded chunk at a
/// time. `entries` is the flattened `(path, oid)` list from
/// `ingest::list_tree_files`, fetched ONCE when the job starts
/// (`start_boot_job`) — tree-object reads only, no blob content, the same
/// cheap read the old unchunked `walk_dir` always did before ever deciding
/// whether to read a blob. `stats` accumulates across chunks
/// (`WalkStats::merge`) so the final "kb-code initial index complete" log
/// line stays byte-identical in shape to the old single-pass total.
struct BootJob {
    repo_id: i64,
    repo_name: String,
    repo_root: PathBuf,
    occurrences_enabled: bool,
    is_rails: bool,
    fingerprints: Arc<HashMap<String, (String, String)>>,
    entries: VecDeque<(String, String)>,
    stats: ingest::WalkStats,
}

/// One slow-lane job in progress — see [`ReconcileJob`]/[`BootJob`].
enum SlowJob {
    Reconcile(ReconcileJob),
    Boot(BootJob),
}

impl SlowJob {
    fn repo_name(&self) -> &str {
        match self {
            SlowJob::Reconcile(j) => &j.repo.name,
            SlowJob::Boot(j) => &j.repo_name,
        }
    }
}

/// The worker loop (V77-P2 rewrite — see the module doc's "Fairness"
/// section for the scheduling contract this implements). Real work
/// (fs/ODB reads, tree-sitter parsing, sqlite writes) always runs via
/// `spawn_blocking`, never inline on this task's own async worker thread —
/// see the pre-V77-P2 module doc note this carries forward: a single slow
/// chunk can still be a couple hundred files, and processing that with no
/// `.await` point in between would starve every OTHER task sharing this
/// runtime's worker threads, HTTP request handling included.
#[allow(clippy::too_many_arguments)]
async fn worker(
    mut fast_rx: mpsc::Receiver<FastMsg>,
    mut slow_rx: mpsc::Receiver<SlowMsg>,
    store: Arc<Store>,
    repo_ids: Arc<HashMap<String, i64>>,
    bus: Arc<EventBus>,
    occurrences: Arc<crate::config::OccurrencesSection>,
    is_rails: Arc<HashMap<String, bool>>,
    // V72-J1 — `[comments] keywords`, resolved ONCE at boot (same
    // no-live-reload posture as `[occurrences]`/`[scopes]`) and threaded to
    // every `index_file` call this worker makes, so the annotation
    // vocabulary can never differ between the boot walk and a later
    // watcher event.
    comment_keywords: Arc<crate::comments::KeywordSet>,
    symbol_index: Arc<crate::search::SymbolIndex>,
    activity: Arc<RepoActivity>,
) {
    let mut processed_total: u64 = 0;
    let mut window = ProgressWindow::new();
    let mut current_job: Option<SlowJob> = None;
    let mut fast_closed = false;
    let mut slow_closed = false;

    loop {
        // 1. Drain up to FAST_BURST_LIMIT fast-lane messages, or until the
        //    lane is momentarily empty — see the module doc's "Fairness"
        //    section.
        for _ in 0..FAST_BURST_LIMIT {
            match fast_rx.try_recv() {
                Ok(msg) => {
                    let queue_len = fast_rx.len();
                    let started = Instant::now();
                    process_fast(
                        msg,
                        &store,
                        &repo_ids,
                        &bus,
                        &occurrences,
                        &is_rails,
                        &comment_keywords,
                    )
                    .await;
                    processed_total += 1;
                    window.record(queue_len, started.elapsed());
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    fast_closed = true;
                    break;
                }
            }
        }

        // 2. If there's no active slow job, try to pick one up
        //    (non-blocking — an empty slow lane must never stall the fast
        //    lane's own next burst).
        if current_job.is_none() {
            match slow_rx.try_recv() {
                Ok(SlowMsg::FullReconcile {
                    repo,
                    changed,
                    removed,
                }) => {
                    current_job = Some(SlowJob::Reconcile(
                        start_reconcile_job(
                            &store,
                            &repo_ids,
                            &occurrences,
                            &is_rails,
                            repo,
                            changed,
                            removed,
                        )
                        .await,
                    ));
                }
                Ok(SlowMsg::BootWalk {
                    repo_id,
                    repo_name,
                    repo_root,
                    occurrences_enabled,
                    is_rails: is_rails_flag,
                }) => {
                    match start_boot_job(
                        &store,
                        repo_id,
                        repo_name.clone(),
                        repo_root,
                        occurrences_enabled,
                        is_rails_flag,
                    )
                    .await
                    {
                        Some(job) => current_job = Some(SlowJob::Boot(job)),
                        None => activity.mark_drained(&repo_name),
                    }
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => slow_closed = true,
            }
            if current_job.is_some() {
                // A job picked up here gets its FIRST chunk only after the
                // next iteration's fast drain: a paired `head_moved` that
                // landed in the fast lane between step 1 and this pick-up
                // must still be observed before the reconcile's first
                // `mirror.updated` chunk — the ordering the module doc's
                // "Fairness" section promises for first AND continuation
                // chunks alike.
                continue;
            }
        }

        // 3. If a slow job is active, give it exactly one chunk this
        //    iteration — always attempted, regardless of how much fast-lane
        //    traffic step 1 just drained (the livelock guard).
        if let Some(job) = current_job.as_mut() {
            let started = Instant::now();
            let done = step_slow_job(job, &store, &bus, &comment_keywords).await;
            processed_total += 1;
            window.record(fast_rx.len(), started.elapsed());
            if done {
                let job = current_job.take().expect("just matched Some");
                let repo_name = job.repo_name().to_string();
                match job {
                    SlowJob::Reconcile(_) => {}
                    SlowJob::Boot(job) => finish_boot_job(job, &store, &symbol_index).await,
                }
                activity.mark_drained(&repo_name);
            }
        } else if !(fast_closed && slow_closed) {
            // Nothing to do right now — block on whichever lane produces
            // something next, rather than spinning.
            tokio::select! {
                biased;
                msg = fast_rx.recv(), if !fast_closed => match msg {
                    Some(msg) => {
                        let queue_len = fast_rx.len();
                        let started = Instant::now();
                        process_fast(msg, &store, &repo_ids, &bus, &occurrences, &is_rails, &comment_keywords).await;
                        processed_total += 1;
                        window.record(queue_len, started.elapsed());
                    }
                    None => fast_closed = true,
                },
                msg = slow_rx.recv(), if !slow_closed => match msg {
                    Some(SlowMsg::FullReconcile { repo, changed, removed }) => {
                        current_job = Some(SlowJob::Reconcile(
                            start_reconcile_job(&store, &repo_ids, &occurrences, &is_rails, repo, changed, removed).await,
                        ));
                    }
                    Some(SlowMsg::BootWalk { repo_id, repo_name, repo_root, occurrences_enabled, is_rails: is_rails_flag }) => {
                        match start_boot_job(&store, repo_id, repo_name.clone(), repo_root, occurrences_enabled, is_rails_flag).await {
                            Some(job) => current_job = Some(SlowJob::Boot(job)),
                            None => activity.mark_drained(&repo_name),
                        }
                    }
                    None => slow_closed = true,
                },
            }
        } else {
            break;
        }

        if window.due() {
            tracing::info!(
                processed_total,
                window_messages = window.messages,
                queue_high_water = window.queue_high_water,
                queue_capacity = QUEUE_CAPACITY,
                window_max_ms = window.max.as_secs_f64() * 1000.0,
                window_avg_ms = window.avg_ms(),
                "kb-code sink worker: progress summary",
            );
            window = ProgressWindow::new();
        }
    }
    tracing::info!(
        "kb-code index sink worker exiting (every IndexSink clone dropped — normal at shutdown; \
         see the module doc's Shutdown-contract section for why queued work isn't drained here)"
    );
}

/// Process exactly one [`FastMsg`] via `spawn_blocking` — unchanged
/// per-message dispatch shape from the pre-V77-P2 worker, just scoped to
/// the fast lane's three variants.
async fn process_fast(
    msg: FastMsg,
    store: &Arc<Store>,
    repo_ids: &Arc<HashMap<String, i64>>,
    bus: &Arc<EventBus>,
    occurrences: &Arc<crate::config::OccurrencesSection>,
    is_rails: &Arc<HashMap<String, bool>>,
    comment_keywords: &Arc<crate::comments::KeywordSet>,
) {
    let store = store.clone();
    let repo_ids = repo_ids.clone();
    let bus = bus.clone();
    let occurrences = occurrences.clone();
    let is_rails = is_rails.clone();
    let comment_keywords = comment_keywords.clone();
    let outcome = tokio::task::spawn_blocking(move || match msg {
        FastMsg::Upsert { repo, path } => {
            let occurrences_enabled = occurrences.repo_enabled(&repo.name);
            let is_rails_flag = is_rails.get(&repo.name).copied().unwrap_or(false);
            handle_upsert(
                &store,
                &repo_ids,
                &bus,
                &repo,
                &path,
                occurrences_enabled,
                is_rails_flag,
                &comment_keywords,
            )
        }
        FastMsg::Remove { repo, path } => handle_remove(&store, &repo_ids, &bus, &repo, &path),
        FastMsg::HeadMoved { repo, old, new } => handle_head_moved(&bus, &repo, old, new),
    })
    .await;
    if let Err(e) = outcome {
        // Only a panic inside the closure (or a runtime shutdown race)
        // reaches here — every `handle_*` fn already logs its own errors
        // internally and never propagates a `Result`.
        tracing::warn!(error = %e, "kb-code sink worker: blocking task panicked");
    }
}

/// Give the active slow job exactly one chunk of work. Returns `true` once
/// the job has no more chunks left (the caller then runs any per-job
/// finish step and marks the repo drained in [`RepoActivity`]).
async fn step_slow_job(
    job: &mut SlowJob,
    store: &Arc<Store>,
    bus: &Arc<EventBus>,
    comment_keywords: &Arc<crate::comments::KeywordSet>,
) -> bool {
    let done = match job {
        SlowJob::Reconcile(job) => step_reconcile_job(job, store, bus, comment_keywords).await,
        SlowJob::Boot(job) => step_boot_job(job, store, bus, comment_keywords).await,
    };
    let delay = test_chunk_delay();
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
    done
}

/// V77-P2 (task 4) test hook — an artificial per-chunk delay, off by
/// default (`Duration::ZERO` unless `KB_CODE_TEST_CHUNK_DELAY_MS` is set to
/// a non-zero, valid `u64`). Exists SOLELY so `tests/boot_e2e`'s
/// `catching_up_is_true_during_the_boot_walk_and_settles_once_drained` (an
/// EXTERNAL integration-test binary — it links this crate as an ordinary
/// dependency, so it cannot reach a `#[cfg(test)]`-gated knob defined
/// inside this crate's own unit-test module) can force the boot walk to
/// take a controlled, host-speed-INDEPENDENT amount of wall time, rather
/// than relying solely on a large fixture file count to "buy enough time"
/// to observe the mid-walk `catching_up: true` state — which flaked hard
/// on a heavily contended shared dev box (measured: real per-file
/// throughput fell from an expected high rate to low single digits/s under
/// concurrent sibling builds fighting for the same disk, blowing even a
/// multi-minute deadline for a several-thousand-file fixture). An env var
/// (rather than a new `spawn` parameter or Cargo feature) is the cheapest
/// way to cross that crate boundary — it needs no threading through
/// `spawn`'s already-long parameter list and every real deployment simply
/// never sets it, so this is a pure no-op (`Duration::ZERO`, one cheap
/// `std::env::var` lookup per chunk) outside that one test. Read fresh
/// per chunk rather than cached once at `spawn` time — the value never
/// changes mid-process in practice (only ever set once, before a test's
/// `boot_with_repo` call), so the repeated lookup costs nothing that
/// matters and avoids adding a field purely for a test's benefit.
fn test_chunk_delay() -> Duration {
    std::env::var("KB_CODE_TEST_CHUNK_DELAY_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or_default()
}

/// Resolve `repo_id`/`occurrences_enabled`/`is_rails` for `repo` exactly as
/// the pre-V77-P2 `worker`'s own `match msg` arm did, then fetch the
/// whole-repo fingerprint map ONCE (V77-P1's own "one query, not one per
/// file" shape) and build the chunked job state for one `FullReconcile`
/// message. An unregistered repo (a race against a config change between
/// boot and this message's send — the pre-V77-P2 code's own edge case)
/// yields a job with an EMPTY op queue rather than one that would try to
/// write under a bogus id: `step_reconcile_job` sees zero ops and reports
/// the job done on its very first (only) step.
async fn start_reconcile_job(
    store: &Arc<Store>,
    repo_ids: &Arc<HashMap<String, i64>>,
    occurrences: &Arc<crate::config::OccurrencesSection>,
    is_rails: &Arc<HashMap<String, bool>>,
    repo: RepoRef,
    changed: Vec<PathBuf>,
    removed: Vec<PathBuf>,
) -> ReconcileJob {
    let occurrences_enabled = occurrences.repo_enabled(&repo.name);
    let is_rails_flag = is_rails.get(&repo.name).copied().unwrap_or(false);
    let Some(&repo_id) = repo_ids.get(&repo.name) else {
        tracing::warn!(
            repo = %repo.name,
            "kb-code sink: reconcile for an unregistered repo — skipping",
        );
        return ReconcileJob {
            repo,
            repo_id: -1,
            fingerprints: Arc::new(HashMap::new()),
            ops: VecDeque::new(),
            occurrences_enabled,
            is_rails: is_rails_flag,
        };
    };

    let store2 = store.clone();
    let fingerprints: HashMap<String, FileRow> = tokio::task::spawn_blocking(move || {
        store2
            .list_files(repo_id)
            .map(|rows| rows.into_iter().map(|f| (f.path.clone(), f)).collect())
            .unwrap_or_default()
    })
    .await
    .unwrap_or_default();

    let mut ops = VecDeque::with_capacity(changed.len() + removed.len());
    ops.extend(changed.into_iter().map(ReconcileOp::Changed));
    ops.extend(removed.into_iter().map(ReconcileOp::Removed));

    ReconcileJob {
        repo,
        repo_id,
        fingerprints: Arc::new(fingerprints),
        ops,
        occurrences_enabled,
        is_rails: is_rails_flag,
    }
}

/// One chunk of a [`ReconcileJob`]. Returns `true` once `ops` is empty.
async fn step_reconcile_job(
    job: &mut ReconcileJob,
    store: &Arc<Store>,
    bus: &Arc<EventBus>,
    comment_keywords: &Arc<crate::comments::KeywordSet>,
) -> bool {
    let take = job.ops.len().min(RECONCILE_CHUNK_SIZE);
    let chunk: Vec<ReconcileOp> = job.ops.drain(..take).collect();
    if chunk.is_empty() {
        return true;
    }
    let repo = job.repo.clone();
    let repo_id = job.repo_id;
    let fingerprints = job.fingerprints.clone();
    let occurrences_enabled = job.occurrences_enabled;
    let is_rails = job.is_rails;
    let store2 = store.clone();
    let comment_keywords2 = comment_keywords.clone();
    let touched = tokio::task::spawn_blocking(move || {
        let mut touched = Vec::new();
        for op in chunk {
            match op {
                ReconcileOp::Changed(rel) => reconcile_one_changed(
                    &store2,
                    &repo,
                    repo_id,
                    &rel,
                    &fingerprints,
                    occurrences_enabled,
                    is_rails,
                    &comment_keywords2,
                    &mut touched,
                ),
                ReconcileOp::Removed(rel) => {
                    reconcile_one_removed(&store2, &repo, repo_id, &rel, &mut touched)
                }
            }
        }
        touched
    })
    .await
    .unwrap_or_default();
    if !touched.is_empty() {
        emit_mirror_updated(bus, &job.repo.name, &touched);
    }
    job.ops.is_empty()
}

/// List the repo's tracked files ONCE (tree-object reads only — no blob
/// content, V77-P2) and build the chunked boot-walk job state. `None` on
/// any failure to open the repo or list its tree (an unborn HEAD, a
/// vanished repo root, …) — logged, matching the old direct boot task's
/// blanket "log and skip this repo" posture.
async fn start_boot_job(
    store: &Arc<Store>,
    repo_id: i64,
    repo_name: String,
    repo_root: PathBuf,
    occurrences_enabled: bool,
    is_rails: bool,
) -> Option<BootJob> {
    let store2 = store.clone();
    let repo_root2 = repo_root.clone();
    let outcome = tokio::task::spawn_blocking(move || -> ingest::Result<_> {
        let git_repo = GitRepo::open(&repo_root2)?;
        let fingerprints: HashMap<String, (String, String)> = store2
            .list_files(repo_id)?
            .into_iter()
            .map(|f| (f.path, (f.blob_hash, f.lang)))
            .collect();
        let entries = ingest::list_tree_files(&git_repo, "HEAD", "")?;
        Ok((fingerprints, entries))
    })
    .await;

    match outcome {
        Ok(Ok((fingerprints, entries))) => Some(BootJob {
            repo_id,
            repo_name,
            repo_root,
            occurrences_enabled,
            is_rails,
            fingerprints: Arc::new(fingerprints),
            entries: entries.into(),
            stats: ingest::WalkStats::default(),
        }),
        Ok(Err(e)) => {
            tracing::warn!(
                repo = %repo_name, error = %e,
                "kb-code sink: boot walk failed to start — skipping",
            );
            None
        }
        Err(e) => {
            tracing::warn!(
                repo = %repo_name, error = %e,
                "kb-code sink: boot walk start task panicked",
            );
            None
        }
    }
}

/// One chunk of a [`BootJob`]. Returns `true` once `entries` is empty.
async fn step_boot_job(
    job: &mut BootJob,
    store: &Arc<Store>,
    bus: &Arc<EventBus>,
    comment_keywords: &Arc<crate::comments::KeywordSet>,
) -> bool {
    let take = job.entries.len().min(RECONCILE_CHUNK_SIZE);
    let chunk: Vec<(String, String)> = job.entries.drain(..take).collect();
    if chunk.is_empty() {
        return true;
    }
    let repo_root = job.repo_root.clone();
    let repo_id = job.repo_id;
    let occurrences_enabled = job.occurrences_enabled;
    let is_rails = job.is_rails;
    let fingerprints = job.fingerprints.clone();
    let store2 = store.clone();
    let comment_keywords2 = comment_keywords.clone();
    let repo_name = job.repo_name.clone();
    let (touched, delta) = tokio::task::spawn_blocking(move || {
        let mut stats = ingest::WalkStats::default();
        let mut touched = Vec::new();
        match GitRepo::open(&repo_root) {
            Ok(repo) => {
                for (path, oid) in &chunk {
                    match ingest::index_one_tree_file(
                        &store2,
                        &repo,
                        repo_id,
                        "HEAD",
                        path,
                        oid,
                        occurrences_enabled,
                        is_rails,
                        &mut stats,
                        &comment_keywords2,
                        &fingerprints,
                    ) {
                        Ok(true) => touched.push(path.clone()),
                        Ok(false) => {}
                        Err(e) => tracing::warn!(
                            repo = %repo_name, path = %path, error = %e,
                            "kb-code sink: boot walk file failed",
                        ),
                    }
                }
            }
            Err(e) => tracing::warn!(
                repo = %repo_name, error = %e,
                "kb-code sink: boot walk chunk failed to reopen the repo",
            ),
        }
        (touched, stats)
    })
    .await
    .unwrap_or_default();

    job.stats.merge(delta);
    if !touched.is_empty() {
        emit_mirror_updated(bus, &job.repo_name, &touched);
    }
    job.entries.is_empty()
}

/// Run once a [`BootJob`] has processed every chunk — mirrors what the old
/// direct boot task (`lib.rs`) used to do inline, right after its own
/// unchunked `ingest::index_repo_working_tree` call returned: rebuild
/// import edges (the walk's own W1.6 "second pass"), log the same
/// completion summary, and warm the shared `SymbolIndex` for this repo.
/// `RepoActivity::mark_drained` is the caller's job, not this fn's — it
/// runs regardless of which `SlowJob` variant just finished (see
/// `worker`'s step 3).
async fn finish_boot_job(
    job: BootJob,
    store: &Arc<Store>,
    symbol_index: &Arc<crate::search::SymbolIndex>,
) {
    let store2 = store.clone();
    let repo_id = job.repo_id;
    match tokio::task::spawn_blocking(move || {
        ingest::rebuild_import_edges_for_repo(&store2, repo_id)
    })
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!(
            repo = %job.repo_name, error = %e,
            "kb-code: boot walk import-edge rebuild failed",
        ),
        Err(e) => tracing::warn!(
            repo = %job.repo_name, error = %e,
            "kb-code: boot walk import-edge rebuild task panicked",
        ),
    }
    tracing::info!(
        repo = %job.repo_name,
        files = job.stats.files,
        parsed = job.stats.parsed,
        cache_hits = job.stats.cache_hits,
        symbols = job.stats.symbols,
        // V72-H2b — the INDEPENDENT highlight gate's own tally, so the cost
        // of a `highlight_salt` bump is a number in the boot log rather
        // than an inference from wall clock.
        highlight_hits = job.stats.highlight_hits,
        highlight_misses = job.stats.highlight_misses,
        highlight_skipped = job.stats.highlight_skipped,
        "kb-code initial index complete",
    );
    let store3 = store.clone();
    let symbol_index2 = symbol_index.clone();
    let repo_id2 = job.repo_id;
    match tokio::task::spawn_blocking(move || symbol_index2.warm(&store3, repo_id2)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!(
            repo = %job.repo_name, error = %e,
            "kb-code: boot-time symbol cache warm failed",
        ),
        Err(e) => tracing::warn!(
            repo = %job.repo_name, error = %e,
            "kb-code: boot-time symbol cache warm task panicked",
        ),
    }
}

/// Repo-relative path as a forward-slash string — the `files.path` /
/// `mirror.updated` payload convention (matches W1.5's own tree-walk, which
/// always produces `/`-joined relative paths regardless of host OS).
fn relativize<'a>(repo: &RepoRef, abs: &'a Path) -> Option<std::borrow::Cow<'a, str>> {
    abs.strip_prefix(&repo.root).ok().map(|rel| {
        if cfg!(windows) {
            std::borrow::Cow::Owned(rel.to_string_lossy().replace('\\', "/"))
        } else {
            rel.to_string_lossy()
        }
    })
}

/// V77-P1 (E6) — the fs-read fast path shared by `handle_upsert` and
/// `reconcile_one_changed`. `row` is the stored fingerprint for this
/// exact path (from `Store::get_file`/`list_files`, both of which now
/// carry `mtime`); `meta` is a `fs::metadata` call the caller already made
/// (never a second stat — see each call site). `true` means "the content
/// is provably unchanged since the write that produced `row`; skip the
/// read+hash+`index_file` entirely."
///
/// `row.mtime == 0` ("unknown" — see `V0044__files_mtime.sql`) can never
/// match: every ODB tree-walk write (`Store::upsert_file`) leaves it at
/// that default, so a file only ever visited through the boot walk before
/// its first live-mirror touch correctly always takes the slow path here
/// once. `Store::is_derived_pair`'s own doc covers the residual TOCTOU
/// between this check succeeding and the caller acting on it.
fn fs_fingerprint_unchanged(store: &Store, row: &FileRow, meta: &std::fs::Metadata) -> bool {
    if row.mtime == 0 {
        return false;
    }
    let Some(mtime) = ingest::mtime_unix_secs(meta) else {
        return false;
    };
    if mtime != row.mtime || meta.len() != row.size {
        return false;
    }
    match crate::lang::for_id(&row.lang) {
        Some(info) => store
            .is_derived_pair(&row.blob_hash, info.symbol_salt, info.highlight_salt)
            .unwrap_or(false),
        // A TIER_* marker (no registered language) has nothing derived to
        // verify — the stored `files` row is already the honest answer.
        None => true,
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_upsert(
    store: &Store,
    repo_ids: &HashMap<String, i64>,
    bus: &EventBus,
    repo: &RepoRef,
    abs_path: &Path,
    occurrences_enabled: bool,
    is_rails: bool,
    comment_keywords: &crate::comments::KeywordSet,
) {
    let Some(&repo_id) = repo_ids.get(&repo.name) else {
        tracing::warn!(repo = %repo.name, "kb-code sink: upsert for an unregistered repo — skipping");
        return;
    };
    let Some(rel) = relativize(repo, abs_path) else {
        tracing::warn!(
            repo = %repo.name, path = %abs_path.display(),
            "kb-code sink: upsert path is not under the repo root — skipping",
        );
        return;
    };

    // V77-P1 — stat BEFORE read, once. On a fingerprint match this avoids
    // the read+hash entirely; on a miss (or no prior row) the SAME
    // metadata's mtime is reused below when writing the fresh fingerprint,
    // so this is never a second syscall for one upsert.
    let meta = std::fs::metadata(abs_path).ok();
    if let Some(meta) = &meta {
        if let Ok(Some(row)) = store.get_file(repo_id, &rel) {
            if fs_fingerprint_unchanged(store, &row, meta) {
                tracing::debug!(
                    repo = %repo.name, path = %rel,
                    "kb-code sink: upsert fingerprint unchanged — skipping read+hash",
                );
                return;
            }
        }
    }

    match std::fs::read(abs_path) {
        Ok(bytes) => {
            let blob_hash = ingest::git_blob_hash(&bytes);
            let mtime = meta.as_ref().and_then(ingest::mtime_unix_secs).unwrap_or(0);
            if let Err(e) = ingest::index_file_with_mtime(
                store,
                repo_id,
                &rel,
                &bytes,
                &blob_hash,
                mtime,
                occurrences_enabled,
                is_rails,
                comment_keywords,
            ) {
                tracing::warn!(repo = %repo.name, path = %rel, error = %e, "kb-code sink: index_file failed");
                return;
            }
            emit_mirror_updated(bus, &repo.name, std::slice::from_ref(&rel.to_string()));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Raced: gone again by the time the worker got to it (e.g. a
            // rapid create-then-delete split across two debounce flushes).
            // A subsequent remove_path/reconcile will clean up the store;
            // nothing to do here.
            tracing::debug!(
                repo = %repo.name, path = %rel,
                "kb-code sink: upsert path vanished before read — skipping",
            );
        }
        Err(e) => {
            tracing::warn!(repo = %repo.name, path = %rel, error = %e, "kb-code sink: read failed");
        }
    }
}

fn handle_remove(
    store: &Store,
    repo_ids: &HashMap<String, i64>,
    bus: &EventBus,
    repo: &RepoRef,
    abs_path: &Path,
) {
    let Some(&repo_id) = repo_ids.get(&repo.name) else {
        tracing::warn!(repo = %repo.name, "kb-code sink: remove for an unregistered repo — skipping");
        return;
    };
    let Some(rel) = relativize(repo, abs_path) else {
        tracing::warn!(
            repo = %repo.name, path = %abs_path.display(),
            "kb-code sink: remove path is not under the repo root — skipping",
        );
        return;
    };
    if let Err(e) = store.delete_file(repo_id, &rel) {
        tracing::warn!(repo = %repo.name, path = %rel, error = %e, "kb-code sink: delete_file failed");
        return;
    }
    emit_mirror_updated(bus, &repo.name, std::slice::from_ref(&rel.to_string()));
}

fn handle_head_moved(
    bus: &EventBus,
    repo: &RepoRef,
    old: Option<gix::ObjectId>,
    new: gix::ObjectId,
) {
    bus.emit(
        "repo.head_moved",
        serde_json::json!({
            "repo": repo.name,
            "old": old.map(|o| o.to_string()),
            "new": new.to_string(),
        }),
    );
}

/// One `Changed` op from a [`ReconcileJob`] chunk — extracted from the
/// pre-V77-P2 `handle_full_reconcile`'s per-path loop body, unchanged in
/// substance, so it can run per-chunk instead of over the whole
/// `changed` set in one pass.
#[allow(clippy::too_many_arguments)]
fn reconcile_one_changed(
    store: &Store,
    repo: &RepoRef,
    repo_id: i64,
    rel: &Path,
    fingerprints: &HashMap<String, FileRow>,
    occurrences_enabled: bool,
    is_rails: bool,
    comment_keywords: &crate::comments::KeywordSet,
    touched: &mut Vec<String>,
) {
    let rel_str = rel.to_string_lossy().to_string();
    let abs = repo.root.join(rel);

    // Stat BEFORE read, once — reused below for the write's mtime on a
    // miss, exactly as `handle_upsert` does.
    let meta = std::fs::metadata(&abs).ok();
    if let (Some(row), Some(meta)) = (fingerprints.get(&rel_str), meta.as_ref()) {
        if fs_fingerprint_unchanged(store, row, meta) {
            return;
        }
    }

    match std::fs::read(&abs) {
        Ok(bytes) => {
            let blob_hash = ingest::git_blob_hash(&bytes);
            let mtime = meta.as_ref().and_then(ingest::mtime_unix_secs).unwrap_or(0);
            if let Err(e) = ingest::index_file_with_mtime(
                store,
                repo_id,
                &rel_str,
                &bytes,
                &blob_hash,
                mtime,
                occurrences_enabled,
                is_rails,
                comment_keywords,
            ) {
                tracing::warn!(
                    repo = %repo.name, path = %rel_str, error = %e,
                    "kb-code sink: reconcile index_file failed",
                );
                return;
            }
            touched.push(rel_str);
        }
        Err(e) => {
            // git reported this path as changed but it isn't readable
            // right now — a rapid follow-up delete, or a path kind ingest
            // doesn't read as file content (a submodule gitlink, reported
            // as its own leaf path per the mirror module's doc, has
            // nothing to read). Not worth surfacing loudly.
            tracing::debug!(
                repo = %repo.name, path = %rel_str, error = %e,
                "kb-code sink: reconcile changed-path read failed — skipping",
            );
        }
    }
}

/// One `Removed` op from a [`ReconcileJob`] chunk — extracted from the
/// pre-V77-P2 `handle_full_reconcile`'s per-path loop body.
fn reconcile_one_removed(
    store: &Store,
    repo: &RepoRef,
    repo_id: i64,
    rel: &Path,
    touched: &mut Vec<String>,
) {
    let rel_str = rel.to_string_lossy().to_string();
    if let Err(e) = store.delete_file(repo_id, &rel_str) {
        tracing::warn!(
            repo = %repo.name, path = %rel_str, error = %e,
            "kb-code sink: reconcile delete_file failed",
        );
        return;
    }
    touched.push(rel_str);
}

fn emit_mirror_updated(bus: &EventBus, repo: &str, paths: &[String]) {
    bus.emit(
        "mirror.updated",
        serde_json::json!({
            "repo": repo,
            "paths": paths,
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    // --- ProgressWindow (A3 observability) ---------------------------------

    #[test]
    fn progress_window_tracks_high_water_and_duration_stats() {
        let mut w = ProgressWindow::new();
        w.record(3, Duration::from_millis(10));
        w.record(7, Duration::from_millis(30));
        w.record(1, Duration::from_millis(20));

        assert_eq!(w.messages, 3);
        // High-water is the MAX queue length seen, not the last or first.
        assert_eq!(w.queue_high_water, 7);
        assert_eq!(w.max, Duration::from_millis(30));
        assert_eq!(w.avg_ms(), 20.0);
    }

    #[test]
    fn progress_window_avg_ms_is_zero_with_no_messages() {
        let w = ProgressWindow::new();
        assert_eq!(w.avg_ms(), 0.0);
        assert!(!w.due());
    }

    #[test]
    fn progress_window_is_due_once_the_message_threshold_is_hit() {
        let mut w = ProgressWindow::new();
        for _ in 0..SUMMARY_EVERY_MESSAGES - 1 {
            w.record(0, Duration::ZERO);
            assert!(!w.due());
        }
        w.record(0, Duration::ZERO);
        assert!(w.due());
    }

    // --- RepoActivity (V77-P2, task 2) --------------------------------------

    #[test]
    fn repo_activity_reports_settled_for_a_repo_never_marked_busy() {
        let activity = RepoActivity::new();
        assert_eq!(activity.snapshot("never-seen"), (false, None));
    }

    #[test]
    fn repo_activity_tracks_busy_and_settles_once_drained() {
        let activity = RepoActivity::new();
        activity.mark_busy("r");
        let (catching_up, settled_at) = activity.snapshot("r");
        assert!(catching_up);
        assert_eq!(settled_at, None, "still walking — settled_at must be null");

        activity.mark_drained("r");
        let (catching_up, settled_at) = activity.snapshot("r");
        assert!(!catching_up);
        assert!(settled_at.is_some(), "drained — settled_at must be set");
    }

    #[test]
    fn repo_activity_stays_busy_while_a_second_job_is_outstanding() {
        // Two overlapping slow-lane messages for the same repo (e.g. a
        // boot walk still running when a live gate-exit reconcile lands) —
        // draining ONE must not report settled while the other is still
        // outstanding.
        let activity = RepoActivity::new();
        activity.mark_busy("r");
        activity.mark_busy("r");
        activity.mark_drained("r");
        assert!(activity.snapshot("r").0, "one job still outstanding");
        activity.mark_drained("r");
        assert!(!activity.snapshot("r").0);
    }

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn init_repo(dir: &Path) {
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["config", "user.email", "t@example.com"]);
        git(dir, &["config", "user.name", "T"]);
    }

    fn setup() -> (tempfile::TempDir, Arc<Store>, HashMap<String, i64>, PathBuf) {
        let repo_tmp = tempfile::tempdir().unwrap();
        let repo_dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
        init_repo(&repo_dir);
        std::fs::write(repo_dir.join("a.rs"), b"fn a() {}\n").unwrap();
        git(&repo_dir, &["add", "-A"]);
        git(&repo_dir, &["commit", "-q", "-m", "c1"]);

        let store_tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&store_tmp.path().join("index.db")).unwrap());
        let repo_id = store
            .upsert_repo("fixture", repo_dir.to_str().unwrap())
            .unwrap();
        let mut repo_ids = HashMap::new();
        repo_ids.insert("fixture".to_string(), repo_id);
        // Keep both tempdirs alive for the caller by leaking repo_tmp's
        // drop guard into the return tuple (store_tmp only needs to outlive
        // `store`, already true via the Arc; repo_tmp must outlive every fs
        // op the test performs against repo_dir).
        (repo_tmp, store, repo_ids, repo_dir)
    }

    /// Every test in this module spawns the SAME shape of sink — a fresh
    /// `SymbolIndex` (never asserted on directly here; `search::symbols`'s
    /// own tests cover `warm`) threaded through so `spawn`'s signature
    /// match is exercised the same way `lib.rs::bind_and_spawn` calls it.
    fn spawn_test_sink(
        store: Arc<Store>,
        repo_ids: HashMap<String, i64>,
        bus: Arc<EventBus>,
    ) -> (IndexSink, Arc<RepoActivity>, tokio::task::JoinHandle<()>) {
        spawn(
            store,
            repo_ids,
            bus,
            crate::config::OccurrencesSection::default(),
            HashMap::new(),
            crate::comments::KeywordSet::defaults(),
            Arc::new(crate::search::SymbolIndex::new()),
        )
    }

    /// `IndexSink`'s `MirrorSink` methods use `blocking_send` (see the
    /// module doc) and MUST NOT be called directly from a tokio runtime
    /// worker — exactly what a `#[tokio::test]` body's own thread is.
    /// Routes every test call through `spawn_blocking`, mirroring how the
    /// REAL caller (`mirror`'s drain thread — a plain `std::thread`, never
    /// a tokio worker) invokes it.
    async fn call_blocking(f: impl FnOnce() + Send + 'static) {
        tokio::task::spawn_blocking(f).await.unwrap();
    }

    #[tokio::test]
    async fn upsert_path_indexes_new_content_and_emits_mirror_updated() {
        let (_repo_tmp, store, repo_ids, repo_dir) = setup();
        let bus = Arc::new(EventBus::default());
        let mut rx = bus.subscribe();
        let (sink, _activity, _handle) =
            spawn_test_sink(store.clone(), repo_ids.clone(), bus.clone());
        let repo_ref = RepoRef {
            name: "fixture".to_string(),
            root: repo_dir.clone(),
        };

        std::fs::write(repo_dir.join("b.rs"), b"fn b() {}\n").unwrap();
        let (sink2, repo_ref2, path) = (sink.clone(), repo_ref.clone(), repo_dir.join("b.rs"));
        call_blocking(move || sink2.upsert_path(&repo_ref2, &path)).await;

        let repo_id = *repo_ids.get("fixture").unwrap();
        // `setup()` only commits "a.rs" to git — it's never pushed through
        // the sink or any walk in THIS test, so the store's only row here
        // is the one this test's own `upsert_path` call produces.
        let ok = wait_for(|| store.file_count(repo_id).unwrap() == 1).await;
        assert!(ok, "expected b.rs to be indexed");
        let file = store.get_file(repo_id, "b.rs").unwrap().unwrap();
        assert_eq!(file.lang, "rust");

        let env = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("event received")
            .unwrap();
        assert_eq!(env.type_, "mirror.updated");
        assert_eq!(env.payload["repo"], "fixture");
        assert_eq!(env.payload["paths"][0], "b.rs");
    }

    #[tokio::test]
    async fn remove_path_deletes_the_files_row() {
        let (_repo_tmp, store, repo_ids, repo_dir) = setup();
        let bus = Arc::new(EventBus::default());
        let (sink, _activity, _handle) = spawn_test_sink(store.clone(), repo_ids.clone(), bus);
        let repo_ref = RepoRef {
            name: "fixture".to_string(),
            root: repo_dir.clone(),
        };
        let repo_id = *repo_ids.get("fixture").unwrap();

        // Seed via upsert first.
        let (sink2, repo_ref2, path) = (sink.clone(), repo_ref.clone(), repo_dir.join("a.rs"));
        call_blocking(move || sink2.upsert_path(&repo_ref2, &path)).await;
        assert!(wait_for(|| store.get_file(repo_id, "a.rs").unwrap().is_some()).await);

        std::fs::remove_file(repo_dir.join("a.rs")).unwrap();
        let (sink2, repo_ref2, path) = (sink.clone(), repo_ref.clone(), repo_dir.join("a.rs"));
        call_blocking(move || sink2.remove_path(&repo_ref2, &path)).await;
        assert!(wait_for(|| store.get_file(repo_id, "a.rs").unwrap().is_none()).await);
    }

    #[tokio::test]
    async fn full_reconcile_indexes_changed_and_deletes_removed() {
        let (_repo_tmp, store, repo_ids, repo_dir) = setup();
        let bus = Arc::new(EventBus::default());
        let (sink, _activity, _handle) = spawn_test_sink(store.clone(), repo_ids.clone(), bus);
        let repo_ref = RepoRef {
            name: "fixture".to_string(),
            root: repo_dir.clone(),
        };
        let repo_id = *repo_ids.get("fixture").unwrap();

        // Seed a second file that reconcile will report as removed.
        let (sink2, repo_ref2, path) = (sink.clone(), repo_ref.clone(), repo_dir.join("a.rs"));
        call_blocking(move || sink2.upsert_path(&repo_ref2, &path)).await;
        assert!(wait_for(|| store.get_file(repo_id, "a.rs").unwrap().is_some()).await);

        std::fs::write(repo_dir.join("c.rs"), b"fn c() {}\n").unwrap();
        std::fs::remove_file(repo_dir.join("a.rs")).unwrap();

        let (sink2, repo_ref2) = (sink.clone(), repo_ref.clone());
        call_blocking(move || {
            MirrorSink::full_reconcile(
                &sink2,
                &repo_ref2,
                vec![PathBuf::from("c.rs")],
                vec![PathBuf::from("a.rs")],
            )
        })
        .await;

        assert!(wait_for(|| store.get_file(repo_id, "c.rs").unwrap().is_some()).await);
        assert!(wait_for(|| store.get_file(repo_id, "a.rs").unwrap().is_none()).await);
    }

    #[tokio::test]
    async fn head_moved_emits_the_expected_payload() {
        let (_repo_tmp, store, repo_ids, repo_dir) = setup();
        let bus = Arc::new(EventBus::default());
        let mut rx = bus.subscribe();
        let (sink, _activity, _handle) = spawn_test_sink(store.clone(), repo_ids.clone(), bus);
        let repo_ref = RepoRef {
            name: "fixture".to_string(),
            root: repo_dir.clone(),
        };
        let new = gix::ObjectId::from_hex(b"0123456789abcdef0123456789abcdef01234567").unwrap();
        let (sink2, repo_ref2) = (sink.clone(), repo_ref.clone());
        call_blocking(move || MirrorSink::head_moved(&sink2, &repo_ref2, None, new)).await;

        let env = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("event received")
            .unwrap();
        assert_eq!(env.type_, "repo.head_moved");
        assert_eq!(env.payload["repo"], "fixture");
        assert!(env.payload["old"].is_null());
        assert_eq!(env.payload["new"], new.to_string());
    }

    async fn wait_for(mut f: impl FnMut() -> bool) -> bool {
        // Generous — `spawn_blocking` work (both the test's own `call_blocking`
        // and the worker's per-message dispatch) queues onto tokio's shared
        // blocking thread pool, which can see real scheduling delay under a
        // heavily loaded CI box.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            if f() {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return f();
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    // --- V77-P1: `fs_fingerprint_unchanged` (task 6) ------------------------

    fn open_bare_store() -> (tempfile::TempDir, Store) {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        (tmp, store)
    }

    fn tmp_file_row(dir: &Path, name: &str, contents: &[u8]) -> (std::path::PathBuf, FileRow) {
        let path = dir.join(name);
        std::fs::write(&path, contents).unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        let row = FileRow {
            path: name.to_string(),
            blob_hash: ingest::git_blob_hash(contents),
            lang: "rust".to_string(),
            size: meta.len(),
            mtime: ingest::mtime_unix_secs(&meta).unwrap(),
        };
        (path, row)
    }

    #[test]
    fn fs_fingerprint_same_mtime_size_and_derived_matches() {
        let (_store_tmp, store) = open_bare_store();
        let tmp = tempfile::tempdir().unwrap();
        let (path, row) = tmp_file_row(tmp.path(), "a.rs", b"fn a() {}\n");
        store
            .mark_derived(
                &row.blob_hash,
                crate::lang::SaltFamily::Symbol,
                crate::lang::RUST.symbol_salt,
                1,
            )
            .unwrap();
        store
            .mark_derived(
                &row.blob_hash,
                crate::lang::SaltFamily::Highlight,
                crate::lang::RUST.highlight_salt,
                0,
            )
            .unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        assert!(fs_fingerprint_unchanged(&store, &row, &meta));
    }

    #[test]
    fn fs_fingerprint_changed_mtime_forces_reread() {
        let (_store_tmp, store) = open_bare_store();
        let tmp = tempfile::tempdir().unwrap();
        let (path, mut row) = tmp_file_row(tmp.path(), "a.rs", b"fn a() {}\n");
        store
            .mark_derived(
                &row.blob_hash,
                crate::lang::SaltFamily::Symbol,
                crate::lang::RUST.symbol_salt,
                1,
            )
            .unwrap();
        store
            .mark_derived(
                &row.blob_hash,
                crate::lang::SaltFamily::Highlight,
                crate::lang::RUST.highlight_salt,
                0,
            )
            .unwrap();
        row.mtime = row.mtime.saturating_add(12345);
        let meta = std::fs::metadata(&path).unwrap();
        assert!(!fs_fingerprint_unchanged(&store, &row, &meta));
    }

    #[test]
    fn fs_fingerprint_mtime_zero_never_matches() {
        let (_store_tmp, store) = open_bare_store();
        let tmp = tempfile::tempdir().unwrap();
        let (path, mut row) = tmp_file_row(tmp.path(), "a.rs", b"fn a() {}\n");
        store
            .mark_derived(
                &row.blob_hash,
                crate::lang::SaltFamily::Symbol,
                crate::lang::RUST.symbol_salt,
                1,
            )
            .unwrap();
        store
            .mark_derived(
                &row.blob_hash,
                crate::lang::SaltFamily::Highlight,
                crate::lang::RUST.highlight_salt,
                0,
            )
            .unwrap();
        row.mtime = 0;
        let meta = std::fs::metadata(&path).unwrap();
        assert!(
            !fs_fingerprint_unchanged(&store, &row, &meta),
            "mtime=0 (\"unknown\") must never be treated as a match"
        );
    }

    #[test]
    fn fs_fingerprint_missing_one_derived_family_forces_reread() {
        let (_store_tmp, store) = open_bare_store();
        let tmp = tempfile::tempdir().unwrap();
        let (path, row) = tmp_file_row(tmp.path(), "a.rs", b"fn a() {}\n");
        // Only the symbol family is marked — highlights never derived.
        store
            .mark_derived(
                &row.blob_hash,
                crate::lang::SaltFamily::Symbol,
                crate::lang::RUST.symbol_salt,
                1,
            )
            .unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        assert!(!fs_fingerprint_unchanged(&store, &row, &meta));
    }

    /// V77-P1 (task 6), end to end: a second `upsert_path` call for a
    /// completely unchanged file must not re-emit `mirror.updated` (proof
    /// that `handle_upsert` actually took the fast path and returned before
    /// ever writing the store or the bus) — proven via a sentinel write
    /// processed strictly AFTER it, since the sink worker is a single
    /// consumer draining its queue in order (module doc).
    #[tokio::test]
    async fn upsert_path_skips_reread_when_fingerprint_is_unchanged() {
        let (_repo_tmp, store, repo_ids, repo_dir) = setup();
        let bus = Arc::new(EventBus::default());
        let mut rx = bus.subscribe();
        let (sink, _activity, _handle) =
            spawn_test_sink(store.clone(), repo_ids.clone(), bus.clone());
        let repo_ref = RepoRef {
            name: "fixture".to_string(),
            root: repo_dir.clone(),
        };
        let repo_id = *repo_ids.get("fixture").unwrap();

        std::fs::write(repo_dir.join("b.rs"), b"fn b() {}\n").unwrap();
        let (sink2, repo_ref2, path) = (sink.clone(), repo_ref.clone(), repo_dir.join("b.rs"));
        call_blocking(move || sink2.upsert_path(&repo_ref2, &path)).await;
        assert!(wait_for(|| store.get_file(repo_id, "b.rs").unwrap().is_some()).await);
        let row1 = store.get_file(repo_id, "b.rs").unwrap().unwrap();
        assert_ne!(row1.mtime, 0, "the fs-read path must record a real mtime");
        // Drain the first upsert's own event before the assertion window.
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await;

        // Second upsert of the SAME unchanged file — should be a no-op.
        let (sink3, repo_ref3, path3) = (sink.clone(), repo_ref.clone(), repo_dir.join("b.rs"));
        call_blocking(move || sink3.upsert_path(&repo_ref3, &path3)).await;

        // A sentinel write, strictly ordered AFTER the call above by the
        // worker's single-consumer queue — once its event lands, the
        // second b.rs upsert has already been fully processed.
        std::fs::write(repo_dir.join("sentinel.rs"), b"fn s() {}\n").unwrap();
        let (sink4, repo_ref4, path4) =
            (sink.clone(), repo_ref.clone(), repo_dir.join("sentinel.rs"));
        call_blocking(move || sink4.upsert_path(&repo_ref4, &path4)).await;

        let mut seen_paths: Vec<String> = Vec::new();
        loop {
            let env = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
                .await
                .expect("sentinel event never arrived")
                .unwrap();
            if env.type_ != "mirror.updated" {
                continue;
            }
            let p = env.payload["paths"][0].as_str().unwrap().to_string();
            seen_paths.push(p.clone());
            if p == "sentinel.rs" {
                break;
            }
        }
        assert!(
            !seen_paths.contains(&"b.rs".to_string()),
            "an unchanged fingerprint must not re-emit mirror.updated for b.rs; saw {seen_paths:?}"
        );

        let row2 = store.get_file(repo_id, "b.rs").unwrap().unwrap();
        assert_eq!(row1, row2, "the unchanged file's row must be untouched");
    }

    // --- V77-P2 (task 1): fairness ------------------------------------------

    /// A fast-lane `Upsert` for an UNRELATED path, sent right after a large
    /// `FullReconcile`, must be reflected (its own `mirror.updated` event
    /// observed) before the reconcile's LAST chunk's own event — proof that
    /// the fast lane isn't stuck behind the whole slow job, only behind at
    /// most `FAST_BURST_LIMIT` fast messages' worth of scheduling per slow
    /// chunk. Deterministic given the scheduling algorithm (see the module
    /// doc's "Fairness" section): it does not depend on any specific wall
    /// time, only on the fast message being enqueued before the reconcile
    /// job's chunks finish draining, which a many-chunk reconcile makes an
    /// extremely generous window.
    #[tokio::test]
    async fn a_fast_upsert_lands_before_an_earlier_queued_reconcile_finishes() {
        let (_repo_tmp, store, repo_ids, repo_dir) = setup();
        let bus = Arc::new(EventBus::default());
        let mut rx = bus.subscribe();
        let (sink, _activity, _handle) =
            spawn_test_sink(store.clone(), repo_ids.clone(), bus.clone());
        let repo_ref = RepoRef {
            name: "fixture".to_string(),
            root: repo_dir.clone(),
        };

        // A reconcile spanning several chunks (a.rs plus enough new files to
        // exceed one RECONCILE_CHUNK_SIZE-sized chunk several times over).
        let total = RECONCILE_CHUNK_SIZE * 3 + 5;
        let mut changed: Vec<PathBuf> = Vec::with_capacity(total);
        for i in 0..total {
            let name = format!("gen_{i}.rs");
            std::fs::write(repo_dir.join(&name), format!("fn f{i}() {{}}\n")).unwrap();
            changed.push(PathBuf::from(name));
        }
        let (sink2, repo_ref2, changed2) = (sink.clone(), repo_ref.clone(), changed.clone());
        call_blocking(move || MirrorSink::full_reconcile(&sink2, &repo_ref2, changed2, Vec::new()))
            .await;

        // Enqueued immediately after — nothing about the reconcile has been
        // dequeued by the worker yet, so this is as early as a live edit
        // realistically arrives relative to a just-started boot/reconcile.
        std::fs::write(repo_dir.join("live_edit.rs"), b"fn live() {}\n").unwrap();
        let (sink3, repo_ref3) = (sink.clone(), repo_ref.clone());
        call_blocking(move || sink3.upsert_path(&repo_ref3, &repo_dir.join("live_edit.rs"))).await;

        let mut seen_reconcile_paths = 0usize;
        let mut live_edit_index: Option<usize> = None;
        let mut events_seen = 0usize;
        loop {
            let env = tokio::time::timeout(std::time::Duration::from_secs(30), rx.recv())
                .await
                .expect("event stream ended before the live edit was observed")
                .unwrap();
            if env.type_ != "mirror.updated" {
                continue;
            }
            events_seen += 1;
            let paths: Vec<String> = env.payload["paths"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect();
            if paths.iter().any(|p| p == "live_edit.rs") {
                live_edit_index = Some(events_seen);
            }
            seen_reconcile_paths += paths.iter().filter(|p| p.starts_with("gen_")).count();
            if live_edit_index.is_some() || seen_reconcile_paths >= total {
                break;
            }
        }

        assert!(
            live_edit_index.is_some(),
            "the live edit's own event never arrived"
        );
        assert!(
            seen_reconcile_paths < total,
            "the live edit landed only after the ENTIRE reconcile had already drained \
             ({seen_reconcile_paths}/{total} paths already touched) — fairness regressed"
        );
    }

    /// The K bound (`FAST_BURST_LIMIT`) holds under a sustained fast-lane
    /// flood: a slow job still gets its chunks processed, rather than
    /// waiting for the whole flood to drain first. The flood re-upserts the
    /// SAME already-indexed, unchanged path 10,000 times — each hit is the
    /// V77-P1 fingerprint skip path (a true no-op that never touches the
    /// store or the bus), so any `mirror.updated` this test observes can
    /// only be the reconcile job's own chunk progress.
    #[tokio::test]
    async fn a_10k_fast_burst_still_lets_a_slow_chunk_through() {
        let (_repo_tmp, store, repo_ids, repo_dir) = setup();
        let bus = Arc::new(EventBus::default());
        let mut rx = bus.subscribe();
        let (sink, _activity, _handle) =
            spawn_test_sink(store.clone(), repo_ids.clone(), bus.clone());
        let repo_ref = RepoRef {
            name: "fixture".to_string(),
            root: repo_dir.clone(),
        };
        let repo_id = *repo_ids.get("fixture").unwrap();
        let a_path = repo_dir.join("a.rs");

        // Index a.rs for real once, so every later re-upsert of it is a
        // true no-op (the V77-P1 fingerprint fast path).
        let (sink2, repo_ref2, path2) = (sink.clone(), repo_ref.clone(), a_path.clone());
        call_blocking(move || sink2.upsert_path(&repo_ref2, &path2)).await;
        assert!(wait_for(|| store.get_file(repo_id, "a.rs").unwrap().is_some()).await);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await;

        // A reconcile with a few chunks' worth of NEW files.
        let total = RECONCILE_CHUNK_SIZE * 2 + 5;
        let mut changed: Vec<PathBuf> = Vec::with_capacity(total);
        for i in 0..total {
            let name = format!("gen_{i}.rs");
            std::fs::write(repo_dir.join(&name), format!("fn f{i}() {{}}\n")).unwrap();
            changed.push(PathBuf::from(name));
        }
        let (sink3, repo_ref3, changed3) = (sink.clone(), repo_ref.clone(), changed.clone());
        call_blocking(move || MirrorSink::full_reconcile(&sink3, &repo_ref3, changed3, Vec::new()))
            .await;

        // Flood the fast lane, concurrently, with 10,000 no-op upserts —
        // mirrors the real producer shape (the watcher's own drain
        // thread hammering `blocking_send` in a tight loop), run on a
        // blocking-pool thread so it genuinely races the worker rather
        // than the test's own async task starving it via cooperative
        // scheduling.
        let flood_sink = sink.clone();
        let flood_repo_ref = repo_ref.clone();
        let flood_path = a_path.clone();
        let flood = tokio::task::spawn_blocking(move || {
            for _ in 0..10_000 {
                flood_sink.upsert_path(&flood_repo_ref, &flood_path);
            }
        });

        let mut saw_reconcile_progress = false;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv()).await {
                Ok(Ok(env)) if env.type_ == "mirror.updated" => {
                    let is_reconcile_progress = env.payload["paths"]
                        .as_array()
                        .map(|arr| {
                            arr.iter()
                                .any(|p| p.as_str().is_some_and(|s| s.starts_with("gen_")))
                        })
                        .unwrap_or(false);
                    if is_reconcile_progress {
                        saw_reconcile_progress = true;
                        break;
                    }
                }
                // Any other event, a lagged broadcast receiver, or a
                // one-second timeout tick — keep polling up to the outer
                // deadline.
                Ok(Ok(_)) | Ok(Err(_)) | Err(_) => {}
            }
        }
        assert!(
            saw_reconcile_progress,
            "a slow-lane chunk should still complete during a 10k-message fast-lane flood",
        );
        flood.await.unwrap();
    }
}
