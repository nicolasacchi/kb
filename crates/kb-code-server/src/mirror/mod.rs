//! Live-mirror watcher (W1.4) — kb-code design doc ADR-3: "watch `.git`,
//! distrust file events, reconcile-don't-replay on git operations."
//!
//! Three sub-steps, one per submodule:
//! - **(a) [`watchset`]** — which paths get registered with `notify`, and
//!   with what recursion mode (the working tree, gitignore-aware pruned,
//!   plus the worktree-correct git-internals set), and which working-tree
//!   events are DROPPED before the sink (`watchset::event_skip_patterns` —
//!   git-ignored directory churn like `.claude/worktrees/` or
//!   `apps/server/log/`, which registration's top-level-only pruning
//!   structurally cannot express).
//! - **(b) [`gate`]** — the per-repo suspend/resume state machine keyed on
//!   `rebase-merge`/`rebase-apply`/`MERGE_HEAD`/`CHERRY_PICK_HEAD`/
//!   `BISECT_LOG` markers, plus the classification that decides what a raw
//!   git-dir-domain path even means.
//! - **(c) [`reconcile`]** — on gate-exit or an idle HEAD move, one
//!   `git diff --name-status` subprocess for the committed delta plus a
//!   stat-then-hash dirty check over whatever working-tree paths were held
//!   during the operation, unioned into exactly one [`MirrorSink::full_reconcile`]
//!   call. The held buffer is always DROPPED after this, never replayed.
//!
//! # Decoupling
//!
//! This module never touches sqlite/lance — every observation crosses the
//! [`MirrorSink`] trait. The real index-backed sink (`crate::sink::
//! IndexSink`, store + `ingest`, wired into daemon boot via
//! `bind_and_spawn`) is W1.6's job; the integration matrix in
//! `tests/mirror_matrix.rs` still uses a recording sink — this module's
//! contract-level behavior (gate/reconcile/dedup) is exactly what that
//! matrix pins, independent of which sink is plugged in.
//!
//! # Suppressing a reconcile's own residual events — and the one case it can't
//!
//! A git operation writes its working-tree files FIRST and only moves
//! `HEAD` as its LAST step, so the two are never atomic with each other —
//! and `notify`'s event delivery can split them across debounce flushes
//! under load (measured: a plain `git checkout` occasionally split its
//! file-writes and its `HEAD` update across two flushes on a heavily loaded
//! box). `process_flush` processes every repo's git-dir-domain events
//! BEFORE its working-tree-domain events within a flush (the common
//! same-flush case is handled for free), and [`POST_RECONCILE_QUIET`]
//! extends that: every reconcile (gate-exit or an idle HEAD move) opens a
//! quiet window on that repo's [`RepoRuntime`], and any working-tree event
//! arriving before the deadline — same flush or a handful of flushes later
//! — is dropped as a presumed residual of the operation just reconciled.
//!
//! What this CANNOT catch, by construction: because the write-then-HEAD
//! order means the working-tree events for an operation can be fully
//! delivered and processed BEFORE the triggering HEAD event ever arrives,
//! there is nothing yet to suppress with — no gate transition and no
//! reconcile has happened for those events to fall inside the quiet window
//! of. A plain (non-marker) operation like `git checkout` therefore has no
//! PRE-signal (unlike a rebase, where the marker directory's creation
//! precedes the risky content writes and sets `suspended` before they can
//! leak — see `gate::RepoGate`). The follow-up reconcile is still always
//! authoritative and correct (it re-derives from `git diff`, not from
//! anything a stray event claimed), so a leaked pre-reconcile upsert is
//! REDUNDANT, never WRONG: it names a path whose on-disk content is already
//! the correct post-operation content. `tests/mirror_matrix.rs`'s checkout
//! case asserts exactly that weaker-but-real invariant (no unrelated path,
//! no repeats) rather than an unconditional zero, which is not achievable
//! under adversarial OS scheduling for an operation with no PRE-signal.

mod gate;
mod reconcile;
mod watchset;

pub use gate::{detect_op, OpDetail, RepoOp, MARKER_NAMES};
pub use kb_core::watcher::WatchMode;
pub use watchset::{git_dir_watch_set, working_tree_watch_set, WatchEntry};

use crate::git::GitRepo;
use anyhow::Context;
use gate::RepoGate;
use notify::event::{EventKind, ModifyKind, RenameMode};
use notify::RecursiveMode;
use notify_debouncer_full::{
    new_debouncer, new_debouncer_opt, DebounceEventResult, DebouncedEvent, RecommendedCache,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Default debounce window (`notify-debouncer-full`'s coalescing window).
pub const DEFAULT_DEBOUNCE_MS: u64 = 250;

/// Identifies one configured repo/worktree to the sink — carries enough
/// (name + canonical worktree root) for a downstream consumer (W1.6) to map
/// back to its own repo registry without re-deriving anything from a raw
/// filesystem path.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RepoRef {
    pub name: String,
    pub root: PathBuf,
}

/// Decoupling contract: the watcher reports through this trait, never
/// touching sqlite/lance directly. A linked worktree and its main worktree
/// (matrix case 5) are DIFFERENT [`RepoRef`]s (different `root`s) even
/// though they share one git object database — every call names the
/// specific repo it's about.
pub trait MirrorSink: Send + Sync + 'static {
    /// This working-tree path should be (re)indexed — a plain edit, or one
    /// leaf of a reconcile's uncommitted-dirty set.
    fn upsert_path(&self, repo: &RepoRef, path: &Path);
    /// This working-tree path is gone.
    fn remove_path(&self, repo: &RepoRef, path: &Path);
    /// `repo`'s resolved HEAD changed from `old` to `new`. `old` is `None`
    /// only at true startup (no prior observation exists yet). Always
    /// followed by exactly one [`Self::full_reconcile`] call for the same
    /// operation.
    fn head_moved(&self, repo: &RepoRef, old: Option<gix::ObjectId>, new: gix::ObjectId);
    /// The authoritative delta for one completed operation (gate-exit, an
    /// idle HEAD move, or the startup bootstrap) — union of the committed
    /// `git diff` delta and the uncommitted dirty check. Both `changed` and
    /// `removed` are repo-relative paths. Never a replay of intermediate
    /// per-file events; this is the ONLY call for that operation's delta.
    fn full_reconcile(&self, repo: &RepoRef, changed: Vec<PathBuf>, removed: Vec<PathBuf>);
}

/// One repo the watcher should mirror.
#[derive(Debug, Clone)]
pub struct RepoWatchConfig {
    pub name: String,
    /// Canonical worktree root (mirrors `kb-code.toml`'s own
    /// `resolve_repos` convention, invariant #27) — `GitRepo::open` is
    /// called on this path directly.
    pub root: PathBuf,
}

#[derive(Debug, Clone)]
pub struct MirrorConfig {
    pub repos: Vec<RepoWatchConfig>,
    pub debounce: Duration,
    pub mode: WatchMode,
    pub poll_interval: Duration,
}

impl MirrorConfig {
    pub fn new(repos: Vec<RepoWatchConfig>) -> Self {
        Self {
            repos,
            debounce: Duration::from_millis(DEFAULT_DEBOUNCE_MS),
            mode: WatchMode::default(),
            poll_interval: Duration::from_millis(kb_core::watcher::DEFAULT_POLL_INTERVAL_MS),
        }
    }

    pub fn with_mode(mut self, mode: WatchMode) -> Self {
        self.mode = mode;
        self
    }

    pub fn with_debounce(mut self, debounce: Duration) -> Self {
        self.debounce = debounce;
        self
    }

    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }
}

/// Parse `kb-code.toml`'s `[watcher] mode` — vocabulary is
/// `"auto"|"notify"|"poll"` (deliberately distinct from `kb_core::watcher`'s
/// own `"auto"|"native"|"poll"`: "notify" names the backend crate this
/// daemon actually links, which reads clearer in kb-code's own config than
/// kb's historical "native"). Unknown/empty falls back to `Auto` with a
/// warning, same shape as `kb_core::watcher::WatchMode::parse`.
pub fn parse_watch_mode(s: &str) -> WatchMode {
    match s.trim().to_ascii_lowercase().as_str() {
        "poll" => WatchMode::Poll,
        "notify" => WatchMode::Native,
        "auto" | "" => WatchMode::Auto,
        other => {
            tracing::warn!(
                value = %other,
                "unrecognised [watcher] mode; falling back to \"auto\" (valid: auto|notify|poll)",
            );
            WatchMode::Auto
        }
    }
}

/// Active watcher — drop the value to stop watching (mirrors
/// `kb_core::watcher::Watcher`'s RAII shape).
pub struct MirrorWatcher {
    _debouncer: HeldDebouncer,
    _drain_thread: std::thread::JoinHandle<()>,
}

#[allow(dead_code)] // held purely for RAII; the inner debouncer is never read back
enum HeldDebouncer {
    Native(notify_debouncer_full::Debouncer<notify::RecommendedWatcher, RecommendedCache>),
    Poll(notify_debouncer_full::Debouncer<notify::PollWatcher, RecommendedCache>),
}

impl MirrorWatcher {
    /// Start watching. Computes the watch-set, arms the debouncer, then
    /// spawns a dedicated `std::thread` (not a tokio worker — the sink's
    /// calls are plain sync fns, and `gix::Repository` isn't `Send`, so
    /// each repo's `GitRepo` is opened fresh ON that thread, never moved
    /// into it) that runs the startup reconcile and then drains live
    /// events for the lifetime of the returned guard.
    pub fn start(config: MirrorConfig, sink: Arc<dyn MirrorSink>) -> anyhow::Result<Self> {
        // Resolve git_dir/common_dir per repo up front on the CALLING
        // thread (a transient GitRepo — dropped before the spawn below, so
        // its non-`Send` `gix::Repository` never has to cross the thread
        // boundary). A repo that fails to open is skipped with a warning
        // rather than failing the whole watcher (mirrors kb-core's A2
        // tolerate-and-continue boot posture).
        let mut boot: Vec<(RepoWatchConfig, PathBuf, PathBuf)> = Vec::new();
        for repo in &config.repos {
            match GitRepo::open(&repo.root) {
                Ok(g) => boot.push((
                    repo.clone(),
                    g.git_dir().to_path_buf(),
                    g.common_dir().to_path_buf(),
                )),
                Err(e) => {
                    tracing::warn!(
                        name = %repo.name, path = %repo.root.display(), error = %e,
                        "mirror watcher: repo does not open — skipping",
                    );
                }
            }
        }

        let mut entries = Vec::new();
        for (repo, git_dir, common_dir) in &boot {
            entries.extend(watchset::working_tree_watch_set(&repo.root));
            entries.extend(watchset::git_dir_watch_set(git_dir, common_dir));
        }
        let entries = watchset::dedup(entries);

        let (debouncer, rx) = arm(&config, &entries)?;

        let drain_thread = std::thread::Builder::new()
            .name("kb-code-mirror".to_string())
            .spawn(move || drain_loop(rx, boot, sink))
            .context("mirror watcher thread spawn")?;

        Ok(Self {
            _debouncer: debouncer,
            _drain_thread: drain_thread,
        })
    }
}

fn arm(
    config: &MirrorConfig,
    entries: &[watchset::WatchEntry],
) -> anyhow::Result<(
    HeldDebouncer,
    std::sync::mpsc::Receiver<DebounceEventResult>,
)> {
    let (tx, rx) = std::sync::mpsc::channel::<DebounceEventResult>();
    let debouncer = if config.mode == WatchMode::Poll {
        let ncfg = notify::Config::default().with_poll_interval(config.poll_interval);
        let mut d = new_debouncer_opt::<_, notify::PollWatcher, _>(
            config.debounce,
            None,
            tx,
            RecommendedCache::new(),
            ncfg,
        )
        .context("notify poll debouncer init")?;
        arm_entries(&mut d, entries);
        HeldDebouncer::Poll(d)
    } else {
        let mut d = new_debouncer(config.debounce, None, tx).context("notify debouncer init")?;
        arm_entries(&mut d, entries);
        HeldDebouncer::Native(d)
    };
    Ok((debouncer, rx))
}

fn arm_entries<T: notify::Watcher>(
    debouncer: &mut notify_debouncer_full::Debouncer<T, RecommendedCache>,
    entries: &[watchset::WatchEntry],
) {
    for entry in entries {
        if !entry.path.exists() {
            // e.g. `.git/logs` before the first ref update on a freshly
            // `git init`'d repo — the HEAD file itself (also watched, via
            // the parent `git_dir` entry) still catches subsequent moves;
            // reconcile is the backstop regardless.
            tracing::debug!(
                path = %entry.path.display(),
                "mirror watcher: watch path does not exist yet — skipping",
            );
            continue;
        }
        let mode = if entry.recursive {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };
        if let Err(e) = debouncer.watch(&entry.path, mode) {
            tracing::warn!(
                path = %entry.path.display(), error = %e,
                "mirror watcher: failed to arm watch",
            );
        }
    }
}

/// A reconcile's own delta is authoritative for the operation that
/// triggered it, but a residual working-tree event from that SAME
/// operation can arrive in a LATER debounce flush than the one that
/// contained the HEAD move/gate-exit (under load, or simply because a
/// real git operation's writes aren't atomic with its HEAD update) — see
/// the module doc's "same-flush coalescing" note. `quiet_until` bridges
/// that gap: any working-tree event for a repo arriving before this
/// deadline is dropped as "presumably already covered by the reconcile
/// that just ran", rather than replayed as an individual upsert/remove.
/// Generous relative to a local git operation's real duration, short
/// relative to human/agent edit cadence. This is best-effort NOISE
/// SUPPRESSION, not a correctness boundary: a residual event arriving after
/// the window closes still lands on an already-correct path (the reconcile
/// already reported it), so the worst case is a redundant, harmless
/// re-upsert — never a wrong or missing path. Under adversarial scheduling
/// (a starved git process can, in principle, take arbitrarily long between
/// writing files and updating `HEAD`) no finite window eliminates that
/// redundant re-upsert entirely; this value is sized for real local git
/// operations under ordinary load, not as a hard guarantee.
const POST_RECONCILE_QUIET: Duration = Duration::from_millis(3000);

/// Per-repo mutable runtime state — the gate plus whatever's needed to act
/// on it (the repo's own paths, the dirty-check cache, the
/// [`POST_RECONCILE_QUIET`] deadline, and the working-tree event skip
/// patterns).
struct RepoRuntime {
    repo_ref: RepoRef,
    git_dir: PathBuf,
    common_dir: PathBuf,
    gate: RepoGate,
    dirty_cache: reconcile::DirtyCache,
    quiet_until: Option<std::time::Instant>,
    /// Working-tree events under these path patterns are dropped before
    /// they can reach the sink (see `watchset::event_skip_patterns`'s doc —
    /// git-ignored directory churn: agent-session worktrees, log/, tmp/,
    /// nested build output). Computed ONCE at watcher start; a `.gitignore`
    /// edit takes effect at the next daemon restart, the same trade
    /// `working_tree_watch_set`'s boot-time registration already makes.
    event_skips: Vec<String>,
}

fn drain_loop(
    rx: std::sync::mpsc::Receiver<DebounceEventResult>,
    boot: Vec<(RepoWatchConfig, PathBuf, PathBuf)>,
    sink: Arc<dyn MirrorSink>,
) {
    let mut runtimes: Vec<RepoRuntime> = Vec::new();
    for (cfg, git_dir, common_dir) in boot {
        let repo_ref = RepoRef {
            name: cfg.name.clone(),
            root: cfg.root.clone(),
        };
        match GitRepo::open(&cfg.root) {
            Ok(repo) => {
                let mut rt = RepoRuntime {
                    repo_ref,
                    git_dir,
                    common_dir,
                    gate: RepoGate::new(),
                    dirty_cache: reconcile::DirtyCache::new(),
                    quiet_until: None,
                    event_skips: watchset::event_skip_patterns(&cfg.root),
                };
                // Startup does an initial full reconcile per repo — "HEAD
                // tree vs nothing = everything, but expressed as the same
                // call" (`committed_delta`'s `old = None` shape).
                startup_reconcile(&mut rt, &repo, sink.as_ref());
                runtimes.push(rt);
            }
            Err(e) => tracing::warn!(
                name = %cfg.name, error = %e,
                "mirror watcher: repo failed to reopen on drain thread",
            ),
        }
    }

    while let Ok(result) = rx.recv() {
        match result {
            Ok(events) => process_flush(&mut runtimes, events, sink.as_ref()),
            Err(errors) => {
                for e in errors {
                    tracing::warn!(error = %e, "mirror watcher error");
                }
            }
        }
    }
    tracing::error!(
        "mirror watcher drain thread exiting (debouncer channel disconnected); no more live \
         events will be observed for any configured repo",
    );
}

fn startup_reconcile(rt: &mut RepoRuntime, repo: &GitRepo, sink: &dyn MirrorSink) {
    let Ok(new_head) = repo.resolve("HEAD") else {
        // Unborn HEAD (freshly `git init`'d, zero commits) — nothing to
        // reconcile; the first real commit will surface as an ordinary
        // idle HEAD move.
        tracing::debug!(
            repo = %rt.repo_ref.name,
            "mirror watcher: unborn HEAD at startup — nothing to reconcile yet",
        );
        return;
    };
    sink.head_moved(&rt.repo_ref, None, new_head);
    let (changed, _removed) = reconcile::committed_delta(repo, &rt.repo_ref.root, None, new_head);
    sink.full_reconcile(&rt.repo_ref, changed, Vec::new());
    rt.gate.last_head = Some(new_head);
}

/// Route + process one debounced flush. Every repo's git-dir-domain events
/// are handled BEFORE any working-tree-domain event in the SAME flush (see
/// the module doc) so a same-flush HEAD move/gate-exit can suppress that
/// flush's own working-tree events instead of emitting them individually.
///
/// Both domains are deduped by `(repo, path)` before processing — a single
/// `notify-debouncer-full` flush can legitimately carry MORE than one raw
/// event for the same path even after its own internal coalescing (e.g. a
/// checkout's content overwrite landing as separate `Data`/`Metadata`
/// kinds); without this, "each path reported once" (matrix case 1's burst
/// property) would only hold by accident of how many raw events the
/// backend happened to emit for a given operation. Last-write-wins per
/// path within the flush (a final Remove beats an earlier Touched, and
/// vice versa) — order-preserving so ties resolve the same way the events
/// themselves arrived.
fn process_flush(runtimes: &mut [RepoRuntime], events: Vec<DebouncedEvent>, sink: &dyn MirrorSink) {
    // Order-preserving dedup by `(repo idx, path)` for BOTH domains: `order`
    // gets a push only the FIRST time a key is seen (the `seen`/`latest` map
    // is the dedup gate), preserving arrival order. This is NOT a cosmetic
    // choice for `git_dir_order` — a sort-based dedup was tried and reverted:
    // when a marker's creation (e.g. `git_dir/rebase-merge`) and a HEAD
    // write land in the SAME flush, their ARRIVAL order is exactly what
    // decides whether the HEAD write is seen while suspended or not (ADR-3's
    // "index zero of a rebase's N intermediate states" — a rebase's very
    // FIRST internal step is itself a HEAD move, and it must be seen as
    // already-suspended). Sorting by path (`"HEAD" < "rebase-merge"`
    // lexicographically) would silently process the HEAD write BEFORE the
    // marker that should have suspended it. `wt_latest` last-write-wins per
    // path within the flush (a final Remove beats an earlier Touched, and
    // vice versa) — also order-preserving.
    let mut git_dir_order: Vec<(usize, PathBuf)> = Vec::new();
    let mut git_dir_seen: std::collections::HashSet<(usize, PathBuf)> =
        std::collections::HashSet::new();
    let mut wt_order: Vec<(usize, PathBuf)> = Vec::new();
    let mut wt_latest: std::collections::HashMap<(usize, PathBuf), bool> =
        std::collections::HashMap::new();

    for event in &events {
        for (path, op) in classify_event_paths(&event.event) {
            route_path(
                runtimes,
                path,
                op,
                &mut git_dir_order,
                &mut git_dir_seen,
                &mut wt_order,
                &mut wt_latest,
            );
        }
    }

    for (idx, path) in git_dir_order {
        handle_git_dir_event(&mut runtimes[idx], &path, sink);
    }

    for key in wt_order {
        let removed = wt_latest[&key];
        let (idx, path) = key;
        let rt = &mut runtimes[idx];
        if is_event_skipped(rt, &path) {
            // Git-ignored directory churn (agent-session worktrees, log/,
            // tmp/, nested build output — see `watchset::event_skip_patterns`).
            // Dropped BEFORE the suspended/quiet dispatch below: ignored
            // churn must neither reach the sink NOR fill the gate's held
            // buffer (which would only make the next reconcile's dirty
            // check stat-and-hash paths git will never report). Filtered
            // here rather than at watch registration because registration
            // can only express "skip this TOP-LEVEL dir" — never
            // `.claude/worktrees/` or `apps/server/log/`.
            continue;
        }
        let quiet = rt
            .quiet_until
            .is_some_and(|deadline| std::time::Instant::now() < deadline);
        if rt.gate.suspended {
            // Held — deduped by path, dropped (never replayed) at the next
            // reconcile.
            rt.gate.held.insert(path);
        } else if quiet {
            tracing::trace!(
                repo = %rt.repo_ref.name, path = %path.display(),
                "mirror watcher: dropping working-tree event inside the post-reconcile quiet \
                 window (presumably a residual write from the operation just reconciled)",
            );
        } else if !removed && path.is_dir() {
            // A directory's own mtime bump (PollWatcher notices a child was
            // added/removed and re-stats the parent too) is not a file
            // content event — never a valid upsert candidate. A REMOVE is
            // exempt from this check by construction: the path is already
            // gone, so `is_dir()` can't observe anything meaningful anyway.
            tracing::trace!(
                repo = %rt.repo_ref.name, path = %path.display(),
                "mirror watcher: dropping a directory-path event (not a file)",
            );
        } else if removed {
            sink.remove_path(&rt.repo_ref, &path);
        } else {
            sink.upsert_path(&rt.repo_ref, &path);
        }
    }
}

/// Attribute one raw path to the repo(s)/domain it belongs to. Git-dir
/// domain (private `git_dir` OR shared `common_dir`) is checked first and
/// can match MULTIPLE repos (the linked-worktree shared-common-dir shape);
/// working-tree domain picks exactly one owner — the longest-prefix
/// `root` match, so a pathologically nested repo root doesn't double-count.
fn route_path(
    runtimes: &[RepoRuntime],
    path: PathBuf,
    op: PathOp,
    git_dir_order: &mut Vec<(usize, PathBuf)>,
    git_dir_seen: &mut std::collections::HashSet<(usize, PathBuf)>,
    wt_order: &mut Vec<(usize, PathBuf)>,
    wt_latest: &mut std::collections::HashMap<(usize, PathBuf), bool>,
) {
    let mut matched_git_dir = false;
    for (idx, rt) in runtimes.iter().enumerate() {
        if path.starts_with(&rt.git_dir) || path.starts_with(&rt.common_dir) {
            let key = (idx, path.clone());
            if git_dir_seen.insert(key.clone()) {
                git_dir_order.push(key);
            }
            matched_git_dir = true;
        }
    }
    if matched_git_dir {
        return;
    }
    let owner = runtimes
        .iter()
        .enumerate()
        .filter(|(_, rt)| path.starts_with(&rt.repo_ref.root))
        .max_by_key(|(_, rt)| rt.repo_ref.root.as_os_str().len());
    if let Some((idx, _)) = owner {
        let key = (idx, path);
        if !wt_latest.contains_key(&key) {
            wt_order.push(key.clone());
        }
        wt_latest.insert(key, op == PathOp::Removed);
    }
}

/// `true` if a working-tree event for `path` (absolute) falls under one of
/// the repo's git-ignored directory patterns — see
/// `watchset::event_skip_patterns`'s doc for the grammar and the sources.
/// The match runs on the repo-relative, forward-slash path (the same
/// convention `sink::relativize` uses) via `kb_core::watcher`'s ONE
/// skip-pattern matcher, so the mirror never grows a second pattern
/// dialect. A path that doesn't strip to a repo-relative form is never
/// skipped here (it will be rejected downstream by `sink::relativize`
/// anyway).
fn is_event_skipped(rt: &RepoRuntime, path: &Path) -> bool {
    if rt.event_skips.is_empty() {
        return false;
    }
    let Ok(rel) = path.strip_prefix(&rt.repo_ref.root) else {
        return false;
    };
    let rel = if cfg!(windows) {
        rel.to_string_lossy().replace('\\', "/")
    } else {
        rel.to_string_lossy().into_owned()
    };
    let basename = rel.rsplit('/').next().unwrap_or(rel.as_str());
    let skipped = rt
        .event_skips
        .iter()
        .any(|pat| kb_core::watcher::path_matches_skip_pattern(&rel, basename, pat));
    if skipped {
        tracing::trace!(
            repo = %rt.repo_ref.name, path = %path.display(),
            "mirror watcher: dropping working-tree event under a git-ignored directory",
        );
    }
    skipped
}

fn handle_git_dir_event(rt: &mut RepoRuntime, path: &Path, sink: &dyn MirrorSink) {
    match gate::classify_git_dir_path(&rt.git_dir, &rt.common_dir, path) {
        gate::GitDirSignal::IndexLockNoise | gate::GitDirSignal::Ignored => {}
        gate::GitDirSignal::Marker => {
            let now_present = gate::markers_present(&rt.git_dir);
            let was_suspended = rt.gate.suspended;
            rt.gate.suspended = now_present;
            if was_suspended && !now_present {
                // Suspended → Idle: gate-exit. Reconcile now, unconditionally
                // (even if HEAD ends up unchanged — e.g. an aborted merge —
                // the held buffer may still carry genuinely dirty paths).
                reconcile_now(rt, sink);
            }
            // Idle → Suspended: nothing else to do here — `last_head`
            // already holds the pre-op boundary (the last time this gate
            // resolved HEAD while idle).
        }
        gate::GitDirSignal::HeadCandidate => {
            if rt.gate.suspended {
                // Distrust every intermediate HEAD during an op — ADR-3:
                // "index zero of a rebase's N intermediate states."
                return;
            }
            let Ok(repo) = GitRepo::open(&rt.repo_ref.root) else {
                return;
            };
            let Ok(new_head) = repo.resolve("HEAD") else {
                return; // still unborn, or a transient resolve race
            };
            if Some(new_head) != rt.gate.last_head {
                let old = rt.gate.last_head;
                sink.head_moved(&rt.repo_ref, old, new_head);
                do_reconcile(rt, &repo, old, new_head, sink);
                rt.gate.last_head = Some(new_head);
            }
        }
    }
}

/// Gate-exit reconcile: resolve the repo's current HEAD (post-operation)
/// and reconcile against whatever it was before the op started
/// (`rt.gate.last_head`, untouched throughout the suspension).
fn reconcile_now(rt: &mut RepoRuntime, sink: &dyn MirrorSink) {
    let Ok(repo) = GitRepo::open(&rt.repo_ref.root) else {
        rt.gate.held.clear();
        return;
    };
    let new_head = repo.resolve("HEAD").ok();
    let old = rt.gate.last_head;
    match new_head {
        Some(new) => {
            if old != Some(new) {
                sink.head_moved(&rt.repo_ref, old, new);
            }
            do_reconcile(rt, &repo, old, new, sink);
            rt.gate.last_head = Some(new);
        }
        None => {
            // The op left the repo unborn (pathological — an aborted
            // rebase/cherry-pick on a repo with zero commits). Nothing to
            // diff against; drop the held buffer, the next real commit
            // will surface as an ordinary idle HEAD move. Still opens a
            // quiet window — the op that just exited may have left residual
            // working-tree writes in flight.
            rt.gate.held.clear();
            rt.quiet_until = Some(std::time::Instant::now() + POST_RECONCILE_QUIET);
        }
    }
}

fn do_reconcile(
    rt: &mut RepoRuntime,
    repo: &GitRepo,
    old: Option<gix::ObjectId>,
    new: gix::ObjectId,
    sink: &dyn MirrorSink,
) {
    let held: Vec<PathBuf> = rt.gate.held.drain().collect();
    let (mut changed, mut removed) = reconcile::committed_delta(repo, &rt.repo_ref.root, old, new);
    let (dirty_changed, dirty_removed) = reconcile::dirty_check(
        &rt.repo_ref.root,
        &held,
        &mut rt.dirty_cache,
        &changed,
        &removed,
    );
    for p in dirty_changed {
        if !changed.contains(&p) {
            changed.push(p);
        }
    }
    for p in dirty_removed {
        if !removed.contains(&p) {
            removed.push(p);
        }
    }
    sink.full_reconcile(&rt.repo_ref, changed, removed);
    // Opens (or extends) the post-reconcile quiet window — see
    // `POST_RECONCILE_QUIET`'s doc.
    rt.quiet_until = Some(std::time::Instant::now() + POST_RECONCILE_QUIET);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathOp {
    Touched,
    Removed,
}

/// Adapted from `kb_core::watcher::dispatch_event`'s hardened
/// Create/Modify/Rename/Remove handling (the v0.7.1 P2 stat-based
/// rename-pair disambiguation, the non-2-path `RenameMode::Both` guard) —
/// collapsed to two outcomes since [`MirrorSink`] only distinguishes
/// upsert vs remove, not create vs modify.
fn classify_event_paths(event: &notify::Event) -> Vec<(PathBuf, PathOp)> {
    match &event.kind {
        EventKind::Create(_) => touched(&event.paths),
        EventKind::Modify(ModifyKind::Data(_))
        | EventKind::Modify(ModifyKind::Metadata(_))
        | EventKind::Modify(ModifyKind::Any)
        | EventKind::Modify(ModifyKind::Other) => touched(&event.paths),
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)) if event.paths.len() == 2 => {
            let (a, b) = (&event.paths[0], &event.paths[1]);
            let (delete, create) = match (a.exists(), b.exists()) {
                (true, false) => (b, a),
                (false, true) => (a, b),
                _ => (a, b),
            };
            vec![
                (delete.clone(), PathOp::Removed),
                (create.clone(), PathOp::Touched),
            ]
        }
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => {
            tracing::warn!(
                paths = ?event.paths,
                "mirror watcher: dropping RenameMode::Both with non-2 path count",
            );
            Vec::new()
        }
        EventKind::Modify(ModifyKind::Name(RenameMode::From)) => removed(&event.paths),
        EventKind::Modify(ModifyKind::Name(RenameMode::To)) => touched(&event.paths),
        EventKind::Remove(_) => removed(&event.paths),
        _ => Vec::new(),
    }
}

fn touched(paths: &[PathBuf]) -> Vec<(PathBuf, PathOp)> {
    paths
        .iter()
        .cloned()
        .map(|p| (p, PathOp::Touched))
        .collect()
}

fn removed(paths: &[PathBuf]) -> Vec<(PathBuf, PathOp)> {
    paths
        .iter()
        .cloned()
        .map(|p| (p, PathOp::Removed))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_watch_mode_handles_kb_code_vocabulary() {
        assert_eq!(parse_watch_mode("poll"), WatchMode::Poll);
        assert_eq!(parse_watch_mode("  NOTIFY "), WatchMode::Native);
        assert_eq!(parse_watch_mode("auto"), WatchMode::Auto);
        assert_eq!(parse_watch_mode(""), WatchMode::Auto);
        assert_eq!(parse_watch_mode("native"), WatchMode::Auto); // kb's own vocab, not kb-code's
    }

    #[test]
    fn mirror_config_defaults() {
        let cfg = MirrorConfig::new(vec![]);
        assert_eq!(cfg.debounce, Duration::from_millis(DEFAULT_DEBOUNCE_MS));
        assert_eq!(cfg.mode, WatchMode::Auto);
        assert!(cfg.repos.is_empty());
    }

    // --- working-tree event skip filter (git-ignored dir churn) -----------

    /// Minimal recording sink — the same shape `tests/mirror_matrix.rs`
    /// uses, re-declared here since integration-test helpers aren't
    /// importable from a unit test.
    #[derive(Default)]
    struct RecordingSink {
        upserts: std::sync::Mutex<Vec<PathBuf>>,
        removes: std::sync::Mutex<Vec<PathBuf>>,
    }

    impl MirrorSink for RecordingSink {
        fn upsert_path(&self, _repo: &RepoRef, path: &Path) {
            self.upserts.lock().unwrap().push(path.to_path_buf());
        }
        fn remove_path(&self, _repo: &RepoRef, path: &Path) {
            self.removes.lock().unwrap().push(path.to_path_buf());
        }
        fn head_moved(&self, _repo: &RepoRef, _old: Option<gix::ObjectId>, _new: gix::ObjectId) {}
        fn full_reconcile(&self, _repo: &RepoRef, _changed: Vec<PathBuf>, _removed: Vec<PathBuf>) {}
    }

    fn wt_modify_event(path: PathBuf) -> DebouncedEvent {
        DebouncedEvent::new(
            notify::Event::new(EventKind::Modify(ModifyKind::Data(
                notify::event::DataChange::Any,
            )))
            .add_path(path),
            std::time::Instant::now(),
        )
    }

    #[test]
    fn process_flush_drops_events_under_git_ignored_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        // The observed shape: a multi-segment agent-worktree pattern plus a
        // bare log/ churn dir — neither expressible at watch registration.
        std::fs::write(root.join(".gitignore"), ".claude/worktrees/\nlog/\n").unwrap();
        let mut runtimes = vec![RepoRuntime {
            repo_ref: RepoRef {
                name: "fixture".to_string(),
                root: root.clone(),
            },
            git_dir: root.join(".git"),
            common_dir: root.join(".git"),
            gate: RepoGate::new(),
            dirty_cache: reconcile::DirtyCache::new(),
            quiet_until: None,
            event_skips: watchset::event_skip_patterns(&root),
        }];
        let sink = RecordingSink::default();

        process_flush(
            &mut runtimes,
            vec![
                wt_modify_event(root.join(".claude/worktrees/agent-x/src/lib.rs")),
                wt_modify_event(root.join("log/app.log")),
                wt_modify_event(root.join("src/lib.rs")),
            ],
            &sink,
        );

        assert_eq!(
            *sink.upserts.lock().unwrap(),
            vec![root.join("src/lib.rs")],
            "only the non-ignored source file may reach the sink",
        );
        assert!(sink.removes.lock().unwrap().is_empty());
    }

    #[test]
    fn process_flush_neither_holds_nor_replays_ignored_churn_during_a_git_op() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        std::fs::write(root.join(".gitignore"), ".claude/worktrees/\n").unwrap();
        let mut rt = RepoRuntime {
            repo_ref: RepoRef {
                name: "fixture".to_string(),
                root: root.clone(),
            },
            git_dir: root.join(".git"),
            common_dir: root.join(".git"),
            gate: RepoGate::new(),
            dirty_cache: reconcile::DirtyCache::new(),
            quiet_until: None,
            event_skips: watchset::event_skip_patterns(&root),
        };
        rt.gate.suspended = true;
        let sink = RecordingSink::default();
        let mut runtimes = vec![rt];

        process_flush(
            &mut runtimes,
            vec![wt_modify_event(
                root.join(".claude/worktrees/agent-x/src/lib.rs"),
            )],
            &sink,
        );

        // Not held for the post-op dirty check, not sent to the sink.
        assert!(runtimes[0].gate.held.is_empty());
        assert!(sink.upserts.lock().unwrap().is_empty());
    }
}
