//! W1.6 — the real [`mirror::MirrorSink`]: turns live-watcher observations
//! into store mutations via `ingest`, off the watcher's dedicated
//! `std::thread` (`mirror::MirrorWatcher::start`'s doc: that thread must
//! never block on parsing).
//!
//! # Shape
//!
//! [`IndexSink`] is a thin, synchronous producer — every `MirrorSink` method
//! is a `blocking_send` onto a bounded `tokio::mpsc` channel (`SinkMsg`).
//! `blocking_send` is legal here for the exact reason
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
//! set into ONE `SinkMsg`, not one per path) — a persistently-full queue
//! means the worker itself is the bottleneck (e.g. parsing a genuinely huge
//! churn), not a burst size this margin should have absorbed.
//!
//! # Events
//!
//! Every store mutation the worker performs also emits onto the shared
//! `kb_core::events::EventBus` (`GET /api/events`, `router.rs`): `"mirror.
//! updated" {repo, paths}` after an upsert/remove/reconcile actually touches
//! the store, and `"repo.head_moved" {repo, old, new}` for every
//! [`mirror::MirrorSink::head_moved`] call (mirrors `mirror::MirrorSink`'s
//! own contract: always followed by exactly one `full_reconcile`, so a
//! `repo.head_moved` frame is always followed by a `mirror.updated` frame
//! for the same operation, in that order, since both are emitted from the
//! SAME single-consumer worker loop).
//!
//! # Observability
//!
//! [`worker`] processes messages one at a time (see "Shape" above), so the
//! only way to tell "keeping up" from "falling behind" from the outside is
//! to watch it: every [`SUMMARY_EVERY_MESSAGES`]-th message (or every
//! [`SUMMARY_EVERY`] of wall time, whichever comes first — so a quiet queue
//! still gets a heartbeat) a `tracing::info!` "progress summary" line
//! reports the window's queue-length high-water mark (sampled via
//! `Receiver::len()` right after each `recv`, i.e. the backlog still
//! waiting behind the message just taken) against [`QUEUE_CAPACITY`], plus
//! the max and average `spawn_blocking` duration for messages processed in
//! that window. This is DETECTION only — no throttling, no alerting wired
//! from it yet — enough for an operator or `kb-code fleet`-equivalent log
//! scrape to notice a bottleneck onset before the queue is actually full.
//! [`ProgressWindow`] is a plain struct so its arithmetic is unit-testable
//! without a running channel.
//!
//! # Shutdown contract — the dropped `JoinHandle` is a decision, not an oversight
//!
//! [`spawn`]'s `JoinHandle` is discarded by every real caller (`lib.rs::
//! bind_and_spawn`) — the worker task runs detached, and whatever `SinkMsg`s
//! are still queued (or mid-`spawn_blocking`) at process shutdown are simply
//! lost with it. This is intentionally NOT a graceful-drain-on-shutdown
//! design. It doesn't need to be: ADR-3's boot sequence (`lib.rs::
//! bind_and_spawn`, see that module's doc) already re-walks every
//! configured repo's `HEAD` tree on EVERY boot — once directly
//! (`sink::initial_index_one`) and again, redundantly but cheaply (ADR-2's
//! blob-hash cache skips re-parsing unchanged content), via the live
//! watcher's own startup reconcile. Any store mutation a dropped queued
//! message would have made is therefore re-derived from scratch on the very
//! next boot, with no replay log or drain barrier required to get there —
//! the same "distrust file events, reconcile-don't-replay" posture the rest
//! of `mirror` is built on, applied to the sink's own shutdown edge.

use crate::git::GitRepo;
use crate::ingest;
use crate::mirror::{MirrorSink, RepoRef};
use crate::store::Store;
use kb_core::events::EventBus;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// See the module doc's "Backpressure choice" section.
pub const QUEUE_CAPACITY: usize = 512;

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
    /// Max `Receiver::len()` observed at dequeue time this window — the
    /// backlog still waiting behind whatever message was just taken.
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

    /// Record one processed message: `queue_len` is the backlog sampled
    /// right after dequeuing it, `elapsed` is that message's own
    /// `spawn_blocking` duration.
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

#[derive(Debug)]
enum SinkMsg {
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
    FullReconcile {
        repo: RepoRef,
        changed: Vec<PathBuf>,
        removed: Vec<PathBuf>,
    },
}

/// The real sink — see the module doc. `Clone` is cheap (an `mpsc::Sender`
/// clone); `mirror::MirrorWatcher::start` takes `Arc<dyn MirrorSink>`, so in
/// practice only one clone is ever made, but nothing stops a future caller
/// (e.g. a manual reindex route) from holding its own clone to push
/// synthetic observations through the same pipeline.
#[derive(Clone)]
pub struct IndexSink {
    tx: mpsc::Sender<SinkMsg>,
}

impl MirrorSink for IndexSink {
    fn upsert_path(&self, repo: &RepoRef, path: &Path) {
        let _ = self.tx.blocking_send(SinkMsg::Upsert {
            repo: repo.clone(),
            path: path.to_path_buf(),
        });
    }

    fn remove_path(&self, repo: &RepoRef, path: &Path) {
        let _ = self.tx.blocking_send(SinkMsg::Remove {
            repo: repo.clone(),
            path: path.to_path_buf(),
        });
    }

    fn head_moved(&self, repo: &RepoRef, old: Option<gix::ObjectId>, new: gix::ObjectId) {
        let _ = self.tx.blocking_send(SinkMsg::HeadMoved {
            repo: repo.clone(),
            old,
            new,
        });
    }

    fn full_reconcile(&self, repo: &RepoRef, changed: Vec<PathBuf>, removed: Vec<PathBuf>) {
        let _ = self.tx.blocking_send(SinkMsg::FullReconcile {
            repo: repo.clone(),
            changed,
            removed,
        });
    }
}

/// Start the worker task and return the [`IndexSink`] handle to feed it —
/// `bind_and_spawn` passes the sink to `mirror::MirrorWatcher::start` and
/// lets the returned `JoinHandle` run detached (a tokio task keeps running
/// once spawned regardless of whether its handle is held; the task's own
/// exit condition — every `IndexSink` clone dropped, closing the channel —
/// only happens at daemon shutdown, when the `MirrorWatcher` itself is
/// dropped too).
/// `is_rails` (PRR-N3) is a per-repo-NAME map, resolved ONCE by the caller
/// (`lib.rs::bind_and_spawn`, via `frameworks::rails::detect_is_rails` +
/// `config::RailsLensSection::repo_enabled`) — a plain `HashMap` lookup per
/// message here, mirroring `repo_ids`'s own shape, deliberately NOT a
/// re-run of the (filesystem-touching) detection itself. See
/// `ingest::index_file`'s doc for why that detection must never happen
/// per-message: this worker drains ONE file-change event at a time, so a
/// `Gemfile`+`routes.rb` re-check on every keystroke-triggered save in an
/// active Rails repo would defeat the whole point of caching it.
pub fn spawn(
    store: Arc<Store>,
    repo_ids: HashMap<String, i64>,
    bus: Arc<EventBus>,
    occurrences: crate::config::OccurrencesSection,
    is_rails: HashMap<String, bool>,
    comment_keywords: crate::comments::KeywordSet,
) -> (IndexSink, tokio::task::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel(QUEUE_CAPACITY);
    let handle = tokio::spawn(worker(
        rx,
        store,
        Arc::new(repo_ids),
        bus,
        Arc::new(occurrences),
        Arc::new(is_rails),
        Arc::new(comment_keywords),
    ));
    (IndexSink { tx }, handle)
}

/// The worker loop. Each message's real work (fs-read + tree-sitter-parse +
/// sqlite-write) is genuinely blocking, so it runs via `spawn_blocking` on
/// tokio's blocking thread pool rather than inline on this task's own async
/// worker thread — that distinction matters once a SINGLE message carries a
/// large batch: a `FullReconcile`'s `changed` set can be an entire repo's
/// worth of paths (the watcher's own startup reconcile walks the whole
/// `HEAD` tree, same as `bind_and_spawn`'s initial-index — see that fn's
/// doc), and processing thousands of files inline with no `.await` point in
/// between would starve every OTHER task sharing this runtime's worker
/// threads for however long the batch takes, HTTP request handling
/// included. `spawn_blocking` keeps this task itself cheap to poll (just
/// awaiting a `JoinHandle`) while the real work happens off the async
/// executor. Messages are still processed ONE AT A TIME, in arrival order —
/// this loop awaits each `spawn_blocking` before draining the next message,
/// preserving the ordering `mirror::MirrorSink`'s contract relies on (a
/// `head_moved` is always immediately followed by its paired
/// `full_reconcile`; processing anything out of order here would let a
/// later message's store write race ahead of an earlier one for the same
/// repo). `blocking_send`'s backpressure (see the module doc) is unaffected
/// either way — the mpsc doesn't care which thread drains it.
async fn worker(
    mut rx: mpsc::Receiver<SinkMsg>,
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
) {
    let mut processed_total: u64 = 0;
    let mut window = ProgressWindow::new();
    while let Some(msg) = rx.recv().await {
        // Backlog still queued behind the message just taken — see the
        // module doc's "Observability" section.
        let queue_len = rx.len();
        let store = store.clone();
        let repo_ids = repo_ids.clone();
        let bus = bus.clone();
        let occurrences = occurrences.clone();
        let is_rails = is_rails.clone();
        let comment_keywords = comment_keywords.clone();
        let started = Instant::now();
        let outcome = tokio::task::spawn_blocking(move || match msg {
            SinkMsg::Upsert { repo, path } => {
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
            SinkMsg::Remove { repo, path } => handle_remove(&store, &repo_ids, &bus, &repo, &path),
            SinkMsg::HeadMoved { repo, old, new } => handle_head_moved(&bus, &repo, old, new),
            SinkMsg::FullReconcile {
                repo,
                changed,
                removed,
            } => {
                let occurrences_enabled = occurrences.repo_enabled(&repo.name);
                let is_rails_flag = is_rails.get(&repo.name).copied().unwrap_or(false);
                handle_full_reconcile(
                    &store,
                    &repo_ids,
                    &bus,
                    &repo,
                    changed,
                    removed,
                    occurrences_enabled,
                    is_rails_flag,
                    &comment_keywords,
                )
            }
        })
        .await;
        let elapsed = started.elapsed();
        processed_total += 1;
        window.record(queue_len, elapsed);
        if let Err(e) = outcome {
            // Only a panic inside the closure (or a runtime shutdown race)
            // reaches here — every `handle_*` fn already logs its own
            // errors internally and never propagates a `Result`.
            tracing::warn!(error = %e, "kb-code sink worker: blocking task panicked");
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
    match std::fs::read(abs_path) {
        Ok(bytes) => {
            let blob_hash = ingest::git_blob_hash(&bytes);
            if let Err(e) = ingest::index_file(
                store,
                repo_id,
                &rel,
                &bytes,
                &blob_hash,
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

#[allow(clippy::too_many_arguments)]
fn handle_full_reconcile(
    store: &Store,
    repo_ids: &HashMap<String, i64>,
    bus: &EventBus,
    repo: &RepoRef,
    changed: Vec<PathBuf>,
    removed: Vec<PathBuf>,
    occurrences_enabled: bool,
    is_rails: bool,
    comment_keywords: &crate::comments::KeywordSet,
) {
    let Some(&repo_id) = repo_ids.get(&repo.name) else {
        tracing::warn!(repo = %repo.name, "kb-code sink: reconcile for an unregistered repo — skipping");
        return;
    };
    let mut touched: Vec<String> = Vec::with_capacity(changed.len() + removed.len());

    // `changed`/`removed` are already repo-relative (see `reconcile::
    // committed_delta`/`dirty_check`'s doc) — read straight off the
    // working tree at `repo.root.join(rel)`, matching the mirror module's
    // own doc: reconcile reports the AUTHORITATIVE delta, and this daemon
    // indexes live working-tree content, not the committed blob (which
    // would miss uncommitted-but-reconciled dirty paths).
    for rel in &changed {
        let rel_str = rel.to_string_lossy().to_string();
        let abs = repo.root.join(rel);
        match std::fs::read(&abs) {
            Ok(bytes) => {
                let blob_hash = ingest::git_blob_hash(&bytes);
                if let Err(e) = ingest::index_file(
                    store,
                    repo_id,
                    &rel_str,
                    &bytes,
                    &blob_hash,
                    occurrences_enabled,
                    is_rails,
                    comment_keywords,
                ) {
                    tracing::warn!(
                        repo = %repo.name, path = %rel_str, error = %e,
                        "kb-code sink: reconcile index_file failed",
                    );
                    continue;
                }
                touched.push(rel_str);
            }
            Err(e) => {
                // git reported this path as changed but it isn't readable
                // right now — a rapid follow-up delete, or a path kind
                // ingest doesn't read as file content (a submodule gitlink,
                // reported as its own leaf path per the mirror module's
                // doc, has nothing to read). Not worth surfacing loudly.
                tracing::debug!(
                    repo = %repo.name, path = %rel_str, error = %e,
                    "kb-code sink: reconcile changed-path read failed — skipping",
                );
            }
        }
    }
    for rel in &removed {
        let rel_str = rel.to_string_lossy().to_string();
        if let Err(e) = store.delete_file(repo_id, &rel_str) {
            tracing::warn!(
                repo = %repo.name, path = %rel_str, error = %e,
                "kb-code sink: reconcile delete_file failed",
            );
            continue;
        }
        touched.push(rel_str);
    }

    if !touched.is_empty() {
        emit_mirror_updated(bus, &repo.name, &touched);
    }
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

/// Open a repo fresh and walk its `HEAD` tree via W1.5's
/// `ingest::index_repo_working_tree` — the "initial background index" boot
/// action (`lib.rs::bind_and_spawn`, item (a) of the W1.6 plan). Kept here
/// (rather than inline in `lib.rs`) since it shares this module's "how do we
/// turn a configured repo into indexed rows" concern, even though it never
/// touches the sink/queue — this is a direct, synchronous store write, run
/// once at boot before the live watcher (and its own startup reconcile) take
/// over. Best-effort per repo: an open/walk failure is logged and does not
/// fail the daemon boot.
pub fn initial_index_one(
    store: &Store,
    repo_id: i64,
    repo_name: &str,
    repo_root: &Path,
    occurrences_enabled: bool,
    is_rails: bool,
    comment_keywords: &crate::comments::KeywordSet,
) {
    match GitRepo::open(repo_root) {
        Ok(git_repo) => match ingest::index_repo_working_tree(
            store,
            &git_repo,
            repo_id,
            "HEAD",
            occurrences_enabled,
            is_rails,
            comment_keywords,
        ) {
            Ok(stats) => tracing::info!(
                repo = %repo_name,
                files = stats.files,
                parsed = stats.parsed,
                cache_hits = stats.cache_hits,
                symbols = stats.symbols,
                "kb-code initial index complete",
            ),
            Err(e) => tracing::warn!(repo = %repo_name, error = %e, "kb-code initial index failed"),
        },
        Err(e) => tracing::warn!(
            repo = %repo_name, error = %e,
            "kb-code initial index: repo does not open — skipping",
        ),
    }
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
        let (sink, _handle) = spawn(
            store.clone(),
            repo_ids.clone(),
            bus.clone(),
            crate::config::OccurrencesSection::default(),
            std::collections::HashMap::new(),
            crate::comments::KeywordSet::defaults(),
        );
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
        let (sink, _handle) = spawn(
            store.clone(),
            repo_ids.clone(),
            bus,
            crate::config::OccurrencesSection::default(),
            std::collections::HashMap::new(),
            crate::comments::KeywordSet::defaults(),
        );
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
        let (sink, _handle) = spawn(
            store.clone(),
            repo_ids.clone(),
            bus,
            crate::config::OccurrencesSection::default(),
            std::collections::HashMap::new(),
            crate::comments::KeywordSet::defaults(),
        );
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
        let (sink, _handle) = spawn(
            store.clone(),
            repo_ids.clone(),
            bus,
            crate::config::OccurrencesSection::default(),
            std::collections::HashMap::new(),
            crate::comments::KeywordSet::defaults(),
        );
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

    #[test]
    fn initial_index_one_populates_the_store() {
        let repo_tmp = tempfile::tempdir().unwrap();
        let repo_dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
        init_repo(&repo_dir);
        std::fs::write(repo_dir.join("a.rs"), b"fn a() {}\n").unwrap();
        git(&repo_dir, &["add", "-A"]);
        git(&repo_dir, &["commit", "-q", "-m", "c1"]);

        let store_tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&store_tmp.path().join("index.db")).unwrap();
        let repo_id = store
            .upsert_repo("fixture", repo_dir.to_str().unwrap())
            .unwrap();

        initial_index_one(
            &store,
            repo_id,
            "fixture",
            &repo_dir,
            true,
            false,
            &crate::comments::KeywordSet::defaults(),
        );
        assert_eq!(store.file_count(repo_id).unwrap(), 1);
        assert_eq!(
            store.get_file(repo_id, "a.rs").unwrap().unwrap().lang,
            "rust"
        );
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
}
