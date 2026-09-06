//! Filesystem watcher built on `notify` + `notify-debouncer-full`.
//!
//! Topic 03 §Decisions: 400 ms debounce window (configurable via
//! `KB_DEBOUNCE_MS`); pin `notify-debouncer-full ≥ 0.7` for the dedup
//! bug #711 fix; recommended backend (inotify on Linux) with a
//! `PollWatcher` fallback for ENOSPC / WSL `/mnt/*` / SMB/NFS.
//!
//! Watches:
//! - the configured source folders (recursive)
//! - the `kb.toml` config file itself (non-recursive) — so `kb add <path>`
//!   writing the file triggers an automatic source-arm without needing a
//!   SIGHUP. Per topic 02 §Decisions.
//!
//! Initial walk on startup: traverses each source with `walkdir`, emits a
//! synthetic `watch.create` for every existing `*.html` file. The indexer
//! treats those as bootstrap-or-resume — if the artifact id matches what's
//! already in lance, no-op; otherwise, index.

use crate::indexer::{IngestSink, WatchKind};
use crate::types::KbName;
use crate::Result;
use notify::event::{EventKind, ModifyKind, RenameMode};
use notify::RecursiveMode;
use notify_debouncer_full::{
    new_debouncer, new_debouncer_opt, DebounceEventResult, RecommendedCache,
};
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const DEFAULT_DEBOUNCE_MS: u64 = 400;
/// Poll interval for `watch_mode = "poll"`. Polling re-stats the tree, so
/// keep it coarse; the reconcile pass is the heavier backstop.
pub const DEFAULT_POLL_INTERVAL_MS: u64 = 2000;

/// Filesystem-watch backend selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WatchMode {
    /// Platform-native backend (inotify), with a WSL `/mnt/*` heads-up
    /// logged at startup.
    #[default]
    Auto,
    /// Pin the native backend, no WSL warning.
    Native,
    /// Force a polling watcher — for filesystems where native events don't
    /// fire: WSL2 `/mnt/*` (DrvFs/9p), SMB/NFS.
    Poll,
}

impl WatchMode {
    /// Parse from config (`auto` | `native` | `poll`); unknown → `Auto`.
    /// An UNRECOGNISED non-empty value logs a warning before defaulting — a
    /// silent fallback would defeat the knob's purpose (a WSL user who typo'd
    /// `watch_mode = "polling"` would get native, no events on `/mnt/*`, and
    /// no hint why).
    pub fn parse(s: &str) -> WatchMode {
        match s.trim().to_ascii_lowercase().as_str() {
            "poll" => WatchMode::Poll,
            "native" => WatchMode::Native,
            "auto" | "" => WatchMode::Auto,
            other => {
                tracing::warn!(
                    value = %other,
                    "unrecognised watch_mode; falling back to \"auto\" (valid: auto|native|poll)",
                );
                WatchMode::Auto
            }
        }
    }
}

/// What the watcher is looking at for a single kb.
#[derive(Debug, Clone)]
pub struct WatcherConfig {
    pub kb_name: KbName,
    /// Source folders to watch recursively for `*.html`.
    pub sources: Vec<PathBuf>,
    /// Optional path to the daemon's `kb.toml` — watched non-recursively so
    /// config edits trigger reload without SIGHUP.
    pub config_file: Option<PathBuf>,
    pub debounce: Duration,
    /// v0.7 — skip-pattern globs (subset of gitignore-style). A file
    /// whose source-relative path matches any pattern is excluded from
    /// the initial walk AND from live event dispatch. See
    /// `path_matches_skip_pattern` for the supported grammar.
    pub skip_patterns: Vec<String>,
    /// Watch backend (native vs poll). See [`WatchMode`].
    pub watch_mode: WatchMode,
    /// Re-stat interval when `watch_mode == Poll`.
    pub poll_interval: Duration,
    /// v0.24 SC1 — stored `(canonical path → mtime_unix)` snapshot, fetched
    /// once at kb bring-up (the same `list_reconcile_rows` projection the
    /// reconcile pass G5-dedups with). The initial walk skips emitting a
    /// file whose on-disk mtime matches its stored mtime, so a daemon
    /// restart over an already-indexed corpus emits (and downstream
    /// re-reads + re-hashes) ~nothing instead of ALL files. A map miss or
    /// mtime mismatch degrades to "emit anyway" — the indexer's
    /// content-hash pre-gate + conditional mtime heal own correctness.
    /// Empty by default (tests, `kb add` bootstrap): full emission,
    /// exactly the pre-SC1 behavior.
    pub known_mtimes: HashMap<PathBuf, i64>,
}

impl WatcherConfig {
    pub fn new(kb_name: KbName, sources: Vec<PathBuf>) -> Self {
        Self {
            kb_name,
            sources,
            config_file: None,
            debounce: Duration::from_millis(DEFAULT_DEBOUNCE_MS),
            skip_patterns: Vec::new(),
            watch_mode: WatchMode::default(),
            poll_interval: Duration::from_millis(DEFAULT_POLL_INTERVAL_MS),
            known_mtimes: HashMap::new(),
        }
    }

    pub fn with_watch_mode(mut self, mode: WatchMode) -> Self {
        self.watch_mode = mode;
        self
    }

    /// Override the `watch_mode = "poll"` re-stat interval (env
    /// `KB_POLL_INTERVAL_MS`). No effect under native backends.
    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    pub fn with_config_file(mut self, path: PathBuf) -> Self {
        self.config_file = Some(path);
        self
    }

    pub fn with_debounce(mut self, debounce: Duration) -> Self {
        self.debounce = debounce;
        self
    }

    pub fn with_skip_patterns(mut self, patterns: Vec<String>) -> Self {
        self.skip_patterns = patterns;
        self
    }

    /// See [`WatcherConfig::known_mtimes`] (v0.24 SC1).
    pub fn with_known_mtimes(mut self, map: HashMap<PathBuf, i64>) -> Self {
        self.known_mtimes = map;
        self
    }
}

/// Active watcher — drop the value to stop watching.
///
/// The debouncer holds the `notify::Watcher`; the JoinHandle drains the
/// debouncer's mpsc into the `EventBus` from a blocking std::thread (the
/// debouncer's tx is std::sync::mpsc, not tokio).
pub struct Watcher {
    _debouncer: HeldDebouncer,
    _drain_thread: std::thread::JoinHandle<()>,
}

/// The watcher backend is chosen at runtime (native vs poll), and the two
/// produce different `Debouncer<T, _>` types — held in this enum so the
/// `Watcher` struct can own either. Both are kept alive purely for RAII
/// (dropping stops watching).
// Held purely for RAII (dropping stops watching); the inner debouncer is
// never read back, hence `dead_code`.
#[allow(dead_code)]
enum HeldDebouncer {
    Native(notify_debouncer_full::Debouncer<notify::RecommendedWatcher, RecommendedCache>),
    Poll(notify_debouncer_full::Debouncer<notify::PollWatcher, RecommendedCache>),
}

/// Arm a debouncer: watch each source recursively + the config file
/// non-recursively. Generic over the watcher backend.
fn arm_debouncer<T: notify::Watcher>(
    debouncer: &mut notify_debouncer_full::Debouncer<T, RecommendedCache>,
    config: &WatcherConfig,
) -> Result<()> {
    for source in &config.sources {
        // A2 — tolerate a not-yet-created source dir. `notify`'s `watch()`
        // returns ENOENT on a missing path; propagating it used to fail the
        // whole daemon boot (and roll back EVERY corpus) the moment an operator
        // added a kb pointing at a folder that didn't exist yet. Skip-and-warn
        // instead: the kb boots empty (initial_walk already skips a missing
        // source), reconcile indexes files once the dir exists, and a restart
        // resumes live file-watching. Adding a kb can no longer brick the daemon.
        if !source.exists() {
            tracing::warn!(
                source = %source.display(),
                "kb source dir does not exist yet — live watching skipped; the kb \
                 boots empty and reconcile indexes files once the dir is created. \
                 Restart the daemon to resume native file-watching.",
            );
            continue;
        }
        debouncer
            .watch(source, RecursiveMode::Recursive)
            .map_err(|e| {
                crate::Error::Storage(format!("notify watch {}: {e}", source.display()))
            })?;
    }
    if let Some(cfg_path) = &config.config_file {
        debouncer
            .watch(cfg_path, RecursiveMode::NonRecursive)
            .map_err(|e| {
                crate::Error::Storage(format!("notify watch config {}: {e}", cfg_path.display()))
            })?;
    }
    Ok(())
}

/// True when running under WSL (real Linux kernel, but `/mnt/*` Windows
/// drives don't deliver inotify events). Best-effort; reads `/proc/version`.
fn is_wsl() -> bool {
    std::fs::read_to_string("/proc/version")
        .map(|s| {
            let l = s.to_ascii_lowercase();
            l.contains("microsoft") || l.contains("wsl")
        })
        .unwrap_or(false)
}

impl Watcher {
    /// Start watching. Performs the initial walk first (so consumers see
    /// `watch.create` for existing files before any live events), then arms
    /// the debouncer.
    pub fn start(config: WatcherConfig, sink: IngestSink) -> Result<Self> {
        // 1. Arm the debouncer. (The initial walk runs first thing on the
        //    drain thread below, NOT here: the sink's `blocking_send` would
        //    panic on a tokio worker, and `start` is called from async
        //    context. Running it on the drain thread keeps the walk's creates
        //    ahead of any live event the debouncer queues in the meantime.)
        let (tx, rx) = std::sync::mpsc::channel::<DebounceEventResult>();
        let debouncer = if config.watch_mode == WatchMode::Poll {
            // Polling backend — re-stats the tree on `poll_interval`. For
            // filesystems where native events don't fire (WSL `/mnt/*`, SMB).
            let ncfg = notify::Config::default().with_poll_interval(config.poll_interval);
            let mut d = new_debouncer_opt::<_, notify::PollWatcher, _>(
                config.debounce,
                None,
                tx,
                RecommendedCache::new(),
                ncfg,
            )
            .map_err(|e| crate::Error::Storage(format!("notify poll debouncer init: {e}")))?;
            arm_debouncer(&mut d, &config)?;
            HeldDebouncer::Poll(d)
        } else {
            let mut d = new_debouncer(config.debounce, None, tx)
                .map_err(|e| crate::Error::Storage(format!("notify debouncer init: {e}")))?;
            arm_debouncer(&mut d, &config)?;
            // WSL heads-up: native inotify silently never fires for sources on
            // Windows drives. Reconcile still catches changes on its interval.
            if config.watch_mode == WatchMode::Auto && is_wsl() {
                for s in &config.sources {
                    if s.starts_with("/mnt/") {
                        tracing::warn!(
                            source = %s.display(),
                            "WSL source under /mnt/* — native inotify does not fire on Windows \
                             drives; set [indexer] watch_mode = \"poll\" or move the corpus into \
                             the Linux filesystem. Reconcile still catches changes on its interval.",
                        );
                    }
                }
            }
            HeldDebouncer::Native(d)
        };

        // 2. Spawn blocking drain thread (runs the initial walk first, then
        // the live-event loop). When the debouncer is dropped, the mpsc
        // senders disconnect and the loop exits naturally. The sink's
        // `blocking_send` is safe here — this is a dedicated std::thread, not
        // a tokio runtime worker.
        let thread_name = format!("kb-watcher-{}", sink.kb());
        let sink_for_thread = sink.clone();
        let sources_for_thread = config.sources.clone();
        let skips_for_thread = config.skip_patterns.clone();
        // SC1 — moved (not cloned): at 20k+ artifacts the map is MBs, and
        // only the drain thread's initial walk consumes it.
        let known_for_thread = config.known_mtimes;
        let drain_thread = std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                drain_loop(
                    rx,
                    sink_for_thread,
                    sources_for_thread,
                    skips_for_thread,
                    known_for_thread,
                )
            })
            .map_err(|e| crate::Error::Storage(format!("watcher thread spawn: {e}")))?;

        Ok(Self {
            _debouncer: debouncer,
            _drain_thread: drain_thread,
        })
    }
}

fn initial_walk(
    source: &Path,
    sink: &IngestSink,
    skip_patterns: &[String],
    known_mtimes: &HashMap<PathBuf, i64>,
) -> Result<()> {
    if !source.exists() {
        return Ok(());
    }
    // X2/D6 — a source paused at boot walks nothing; resuming later is caught
    // by the reconcile safety net (its next tick re-walks the source). The
    // per-item `prepare_doc` gate backstops any race regardless.
    if sink.gate().paused() {
        return Ok(());
    }
    for entry in walkdir::WalkDir::new(source)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        // X1 — the ingest gate is the sink's resolved extension map (default
        // set unless `bring_up_kb` installed a per-kb override).
        if !sink.extensions().is_indexable(path) {
            continue;
        }
        if is_skipped(path, source, skip_patterns) {
            continue;
        }
        // X2 — operator-excluded files never enter the ingest channel.
        if sink.gate().is_excluded(path, source) {
            continue;
        }
        // SC1 — G5-style producer-side dedup, the reconcile walk's exact
        // contract (`indexer::walk_core`): skip emitting a file whose
        // on-disk mtime matches the stored mtime from its last index. On a
        // restart over a fully-indexed corpus this walk emitted one create
        // per file — each a downstream read+hash, and (pre-SC1) a
        // full-table-scan `touch_mtime` UPDATE commit — the storm's feeder
        // #1. Stat-only (walkdir's cached metadata); a map miss or
        // mismatch emits as before, and the indexer's content-hash
        // pre-gate + conditional heal keep correctness downstream.
        if !known_mtimes.is_empty() {
            if let Some(&stored) = known_mtimes.get(path) {
                let disk_mtime = entry
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64);
                if disk_mtime == Some(stored) {
                    continue;
                }
            }
        }
        sink.blocking_send(WatchKind::Created, path.to_path_buf(), false);
    }
    Ok(())
}

/// True if `path`'s source-relative form matches any pattern in
/// `skip_patterns`. Empty patterns never match; returns false on empty
/// skip list.
pub fn is_skipped(path: &Path, source: &Path, skip_patterns: &[String]) -> bool {
    if skip_patterns.is_empty() {
        return false;
    }
    let Some(rel) = relative_str(path, source) else {
        return false;
    };
    let basename = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    skip_patterns
        .iter()
        .any(|pat| path_matches_skip_pattern(&rel, basename, pat))
}

/// Compute the forward-slash relative path of `abs` against `base`.
/// Returns None when `abs` isn't under `base`.
fn relative_str(abs: &Path, base: &Path) -> Option<String> {
    let rel = abs.strip_prefix(base).ok()?;
    Some(
        rel.components()
            .filter_map(|c| c.as_os_str().to_str())
            .collect::<Vec<_>>()
            .join("/"),
    )
}

/// Lightweight gitignore-ish matcher. Supported pattern shapes:
///
/// - `name`             — exact relative path OR exact basename match
/// - `*.ext`            — basename ends with `.ext`
/// - `prefix/**`        — relative path starts with `prefix/` (or equals `prefix`)
/// - `**/middle/**`     — relative path contains `/middle/` (or starts with `middle/`)
/// - `**/leaf`          — basename equals `leaf`
///
/// Everything else is matched as a literal substring against the relative
/// path. Wildcard semantics intentionally stop at the four common shapes
/// above; if you need a real gitignore engine, add the `ignore` crate.
pub fn path_matches_skip_pattern(rel: &str, basename: &str, pattern: &str) -> bool {
    let pat = pattern.trim();
    if pat.is_empty() {
        return false;
    }

    // Shape: `**/middle/**`  → contains `/middle/`.
    if let Some(inner) = pat.strip_prefix("**/").and_then(|s| s.strip_suffix("/**")) {
        if inner.is_empty() {
            return false;
        }
        let needle = format!("/{inner}/");
        let needle_no_lead = format!("{inner}/");
        return rel.contains(&needle) || rel.starts_with(&needle_no_lead) || rel == inner;
    }

    // Shape: `prefix/**`  → starts with `prefix/` or equals `prefix`.
    if let Some(prefix) = pat.strip_suffix("/**") {
        if prefix.is_empty() {
            return false;
        }
        return rel == prefix || rel.starts_with(&format!("{prefix}/"));
    }

    // Shape: `**/leaf`  → basename equals leaf.
    if let Some(leaf) = pat.strip_prefix("**/") {
        if leaf.is_empty() {
            return false;
        }
        return basename == leaf;
    }

    // Shape: `*.ext`  → basename ends with `.ext`. `len() > ext.len()`
    // (not `>= ext.len() + 1`): a file literally named `.tmp` — an empty
    // stem, basename len 4, ext "tmp" len 3 — should still match `*.tmp`
    // (v0.7.1 P2). The pre-P2 `> ext.len() + 1` rejected it.
    if let Some(ext) = pat.strip_prefix("*.") {
        return basename.len() > ext.len() && basename.ends_with(&format!(".{ext}"));
    }

    // Exact-name match: either the whole relative path or the basename.
    rel == pat || basename == pat
}

fn drain_loop(
    rx: std::sync::mpsc::Receiver<DebounceEventResult>,
    sink: IngestSink,
    sources: Vec<PathBuf>,
    skip_patterns: Vec<String>,
    known_mtimes: HashMap<PathBuf, i64>,
) {
    let kb_name = sink.kb().clone();

    // Initial walk — synthetic watch.create per existing indexable file,
    // minus skip_patterns, minus already-indexed-and-unchanged files (the
    // SC1 known_mtimes gate). Runs here (the drain thread) so the sink's
    // `blocking_send` back-pressures legally off the tokio runtime, and ahead
    // of the recv loop so existing-file creates precede any live event the
    // debouncer has already queued.
    for source in &sources {
        if let Err(e) = initial_walk(source, &sink, &skip_patterns, &known_mtimes) {
            tracing::warn!(kb = %kb_name, source = %source.display(), error = %e, "watcher initial walk failed");
        }
    }
    // The snapshot served the walk; free it before the long-lived recv loop.
    drop(known_mtimes);

    while let Ok(result) = rx.recv() {
        match result {
            Ok(events) => {
                for event in events {
                    dispatch_event(&sink, &event.event, &sources, &skip_patterns);
                }
            }
            Err(errors) => {
                for e in errors {
                    tracing::warn!(kb = %kb_name, error = %e, "watcher error");
                    // R1 — inotify queue overflow means the watcher
                    // silently dropped events. The reconciler will
                    // eventually catch up,
                    // but operators want to know the daemon noticed AND
                    // self-recovered. Match on the error string because
                    // notify-debouncer-full doesn't expose a typed
                    // overflow variant uniformly across platforms.
                    let s = e.to_string().to_lowercase();
                    if s.contains("overflow") || s.contains("lagged") {
                        tracing::warn!(
                            kb = %kb_name,
                            reason = %e,
                            "watcher queue overflow detected — triggering catch-up rescan",
                        );
                        // `watcher.lagged` is observability-only (not ingest),
                        // so it goes straight to the bus, not the sink channel.
                        sink.bus().emit(
                            "watcher.lagged",
                            json!({
                                "kb": kb_name.as_str(),
                                "reason": e.to_string(),
                                "ts": std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_secs() as i64)
                                    .unwrap_or(0),
                            }),
                        );
                        // Spawn the rescan off this thread so we don't block
                        // subsequent debounce results. Re-walk pushes
                        // `watch.modify` (force=false) through the sink — the
                        // indexer's dedup gate keeps byte-identical files cheap,
                        // and the bounded channel back-pressures the rescan.
                        let sink_for_rescan = sink.clone();
                        let sources_for_rescan = sources.clone();
                        let skips_for_rescan = skip_patterns.clone();
                        std::thread::spawn(move || {
                            for src in &sources_for_rescan {
                                // Overflow recovery re-asserts every file (no
                                // mtime map — on an inotify-overflow we'd rather
                                // re-emit all than risk skipping a missed change;
                                // the content-hash pre-gate keeps it cheap).
                                // "Cheap" is only true since SC1 made the
                                // pre-gate's mtime heal conditional on disk !=
                                // stored — before that, every re-asserted
                                // unchanged file fired a full-table-scan
                                // `touch_mtime` UPDATE commit (the v0.24
                                // restart/backlog storm), and this rescan was
                                // one of the three duplicate-emission feeders.
                                crate::indexer::walk_send_work(
                                    &sink_for_rescan,
                                    src,
                                    &skips_for_rescan,
                                    false,
                                    None,
                                );
                            }
                        });
                    }
                }
            }
        }
    }
    // Reached when the debouncer's mpsc senders disconnect — typically
    // because the `Watcher` value was dropped (clean shutdown). If it
    // happens while the daemon is still up, the watcher is now dead and
    // only the reconciliation pass will pick up filesystem changes; log
    // ERROR so the operator can see it in `journalctl`/daemon logs.
    tracing::error!(
        kb = %kb_name,
        "watcher drain thread exiting (notify mpsc disconnected); reconciler is now the sole source of fs events",
    );
}

fn dispatch_event(
    sink: &IngestSink,
    event: &notify::Event,
    sources: &[PathBuf],
    skip_patterns: &[String],
) {
    tracing::debug!(
        kb = %sink.kb(),
        kind = ?event.kind,
        paths = ?event.paths,
        "watcher dispatch",
    );
    match &event.kind {
        EventKind::Create(_) => emit_for_paths(
            sink,
            WatchKind::Created,
            &event.paths,
            sources,
            skip_patterns,
        ),
        // `Data`/`Metadata` are what inotify emits for a write. The
        // `Any`/`Other` arms are defensive catch-alls for a coarse backend
        // (e.g. `PollWatcher`'s `Modify(Data(Any))`, or a future notify
        // rev that coalesces a write into a bare `Modify(Any)`/`Modify(Other)`)
        // so such a write isn't silently dropped (`_ => {}`) until the
        // reconcile pass. inotify never emits a bare `Any`, so the extra
        // arms don't change Linux behaviour; re-index is content-hash
        // idempotent, so an over-broad match is harmless even if one
        // fires spuriously.
        EventKind::Modify(ModifyKind::Data(_))
        | EventKind::Modify(ModifyKind::Metadata(_))
        | EventKind::Modify(ModifyKind::Any)
        | EventKind::Modify(ModifyKind::Other) => {
            emit_for_paths(
                sink,
                WatchKind::Modified,
                &event.paths,
                sources,
                skip_patterns,
            );
        }
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)) if event.paths.len() != 2 => {
            // LOW (deep-review): some `notify` backends deliver a Both
            // rename with 1 path or 3+ paths (rare). On Linux/inotify
            // it's always 2, but we don't want to silently swallow the
            // off-path case — the reconciler will eventually catch up,
            // but the operator should know it happened.
            tracing::warn!(
                kb = %sink.kb(),
                paths = ?event.paths,
                "watcher: dropping RenameMode::Both with non-2 path count (unhandled by linux/inotify path)"
            );
        }
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)) if event.paths.len() == 2 => {
            // v0.7.1 P2 — `notify` does NOT guarantee paths[0] is the
            // rename source and paths[1] the destination; the ordering
            // is backend-dependent. Stat both: the path that now exists
            // is the destination (→ watch.create), the one that's gone
            // is the source (→ watch.delete). If both or neither exist
            // (a same-path event, or a race), fall back to positional.
            let (a, b) = (&event.paths[0], &event.paths[1]);
            let (delete, create) = match (a.exists(), b.exists()) {
                (true, false) => (b, a),
                (false, true) => (a, b),
                _ => (a, b),
            };
            emit_for_paths(
                sink,
                WatchKind::Deleted,
                std::slice::from_ref(delete),
                sources,
                skip_patterns,
            );
            emit_for_paths(
                sink,
                WatchKind::Created,
                std::slice::from_ref(create),
                sources,
                skip_patterns,
            );
        }
        EventKind::Modify(ModifyKind::Name(RenameMode::From)) => {
            emit_for_paths(
                sink,
                WatchKind::Deleted,
                &event.paths,
                sources,
                skip_patterns,
            );
        }
        EventKind::Modify(ModifyKind::Name(RenameMode::To)) => {
            emit_for_paths(
                sink,
                WatchKind::Created,
                &event.paths,
                sources,
                skip_patterns,
            );
        }
        EventKind::Remove(_) => emit_for_paths(
            sink,
            WatchKind::Deleted,
            &event.paths,
            sources,
            skip_patterns,
        ),
        _ => {} // Access, Other, Any — ignored
    }
}

fn emit_for_paths(
    sink: &IngestSink,
    kind: WatchKind,
    paths: &[PathBuf],
    sources: &[PathBuf],
    skip_patterns: &[String],
) {
    for path in paths {
        // X1 — the ingest gate is the sink's resolved extension map. Config
        // files (kb.toml/daemons.toml) remain a SEPARATE concern: they're
        // never artifacts but the daemon must react to their edits regardless
        // of the map, so `is_config_file` keeps its own bypass (see #7 below).
        let indexable = sink.extensions().is_indexable(path);
        if !indexable && !is_config_file(path) {
            continue;
        }
        // Config files bypass skip_patterns AND the X2 gate below since
        // they're not artifacts and the daemon needs to react to their
        // changes regardless of any path filter or pause.
        if indexable {
            if path_skipped_under_any_source(path, sources, skip_patterns) {
                continue;
            }
            // X2/D6 — a paused source's live events (deletes included) are
            // dropped wholesale: paused freezes the kb's pipeline; resume +
            // the next reconcile tick heal anything that changed meanwhile.
            if sink.gate().paused() {
                continue;
            }
            // X2 — events for operator-excluded files are ignored.
            if sink.gate().is_excluded_under_any(path, sources) {
                continue;
            }
        }
        sink.blocking_send(kind, path.to_path_buf(), false);
    }
}

/// True if `path` matches `skip_patterns` relative to ANY source root.
/// Live notify events arrive with absolute paths; the watcher pairs them
/// with the configured sources to compute the source-relative form.
fn path_skipped_under_any_source(
    path: &Path,
    sources: &[PathBuf],
    skip_patterns: &[String],
) -> bool {
    if skip_patterns.is_empty() {
        return false;
    }
    sources
        .iter()
        .any(|source| is_skipped(path, source, skip_patterns))
}

fn is_config_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s == "kb.toml" || s == "daemons.toml")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::EventBus;
    use std::fs;
    use std::sync::Arc;
    use std::time::Instant;
    use tokio::time::timeout;

    fn kb() -> KbName {
        KbName::new("smoke").unwrap()
    }

    #[test]
    fn watch_mode_parse_handles_case_whitespace_and_unknown() {
        assert_eq!(WatchMode::parse("poll"), WatchMode::Poll);
        assert_eq!(WatchMode::parse("  POLL "), WatchMode::Poll);
        assert_eq!(WatchMode::parse("Native"), WatchMode::Native);
        assert_eq!(WatchMode::parse("auto"), WatchMode::Auto);
        // empty + unrecognised both fall back to Auto (the latter also warns).
        assert_eq!(WatchMode::parse(""), WatchMode::Auto);
        assert_eq!(WatchMode::parse("polling"), WatchMode::Auto);
    }

    #[test]
    fn with_poll_interval_overrides_default() {
        let cfg = WatcherConfig::new(kb(), vec![])
            .with_watch_mode(WatchMode::Poll)
            .with_poll_interval(Duration::from_millis(250));
        assert_eq!(cfg.poll_interval, Duration::from_millis(250));
        assert_eq!(cfg.watch_mode, WatchMode::Poll);
    }

    /// An `IngestSink` for watcher tests. These assert on the bus MIRROR (the
    /// sink emits `watch.*` to the bus before pushing to the channel), so the
    /// ingest receiver is intentionally dropped — `blocking_send` then fails
    /// silently after the mirror has already fired. (A real consumer is tested
    /// separately in `indexer.rs`.)
    fn test_sink(bus: &Arc<EventBus>) -> IngestSink {
        let (tx, _rx) = tokio::sync::mpsc::channel(crate::indexer::INGEST_QUEUE_CAPACITY);
        IngestSink::new(tx, bus.clone(), kb())
    }

    /// Wait up to 5 s for an envelope of the given type to appear.
    async fn next_typed(
        rx: &mut tokio::sync::broadcast::Receiver<crate::types::Envelope>,
        type_: &str,
    ) -> Option<crate::types::Envelope> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            match timeout(Duration::from_millis(200), rx.recv()).await {
                Ok(Ok(env)) if env.type_ == type_ => return Some(env),
                Ok(Ok(_)) => continue,
                Ok(Err(_)) | Err(_) => continue,
            }
        }
        None
    }

    #[tokio::test]
    async fn initial_walk_emits_create_for_existing_html() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("a.html"), "<html></html>").unwrap();
        fs::write(tmp.path().join("b.html"), "<html></html>").unwrap();
        fs::write(tmp.path().join("ignore.txt"), "no").unwrap();

        let bus = Arc::new(EventBus::default());
        let _w = Watcher::start(
            WatcherConfig::new(kb(), vec![tmp.path().to_path_buf()])
                .with_debounce(Duration::from_millis(50)),
            test_sink(&bus),
        )
        .unwrap();

        // The initial walk now runs on the watcher's drain thread (so the
        // sink's blocking_send back-pressures off the tokio runtime). Poll the
        // bus replay ring until both creates land.
        let creates = timeout(Duration::from_secs(5), async {
            loop {
                let n = bus
                    .snapshot_since(0)
                    .iter()
                    .filter(|e| e.type_ == "watch.create")
                    .count();
                if n >= 2 {
                    return n;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("initial-walk creates did not appear");
        assert_eq!(creates, 2, "two .html files should yield two creates");
    }

    /// X2 — the watcher's initial walk skips operator-excluded files (the
    /// gate rides the sink, exactly like skip_patterns ride the config).
    #[tokio::test]
    async fn initial_walk_skips_excluded_files() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("keep.html"), "<html></html>").unwrap();
        fs::write(tmp.path().join("drop.html"), "<html></html>").unwrap();

        let bus = Arc::new(EventBus::default());
        let gate = crate::exclusions::IngestGate::default();
        gate.add_excluded("drop.html");
        let _w = Watcher::start(
            WatcherConfig::new(kb(), vec![tmp.path().to_path_buf()])
                .with_debounce(Duration::from_millis(50)),
            test_sink(&bus).with_gate(gate),
        )
        .unwrap();

        let creates = timeout(Duration::from_secs(5), async {
            loop {
                let creates: Vec<_> = bus
                    .snapshot_since(0)
                    .into_iter()
                    .filter(|e| e.type_ == "watch.create")
                    .collect();
                if !creates.is_empty() {
                    return creates;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("initial-walk create did not appear");
        assert_eq!(creates.len(), 1, "the excluded file must not walk");
        assert!(creates[0].payload["path"]
            .as_str()
            .unwrap()
            .ends_with("keep.html"));
    }

    /// SC1 — the initial walk's known-mtimes gate (the restart storm's
    /// feeder #1): a file whose disk mtime matches its stored mtime is
    /// NOT emitted; a mismatched or unknown file still is. Pre-SC1 every
    /// restart emitted ALL files, each one a downstream read+hash and an
    /// unconditional `touch_mtime` UPDATE commit.
    #[tokio::test]
    async fn initial_walk_skips_files_with_matching_stored_mtime() {
        let tmp = tempfile::tempdir().unwrap();
        for name in ["same.html", "touched.html", "unknown.html"] {
            fs::write(tmp.path().join(name), "<html></html>").unwrap();
        }
        let mtime_of = |name: &str| -> i64 {
            fs::metadata(tmp.path().join(name))
                .unwrap()
                .modified()
                .unwrap()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64
        };
        let mut known = HashMap::new();
        // Matches disk → must be skipped.
        known.insert(tmp.path().join("same.html"), mtime_of("same.html"));
        // Stored ≠ disk (the git-checkout / genuinely-touched shape) → emits.
        known.insert(
            tmp.path().join("touched.html"),
            mtime_of("touched.html") - 7,
        );
        // unknown.html absent from the map → emits.

        let bus = Arc::new(EventBus::default());
        let _w = Watcher::start(
            WatcherConfig::new(kb(), vec![tmp.path().to_path_buf()])
                .with_debounce(Duration::from_millis(50))
                .with_known_mtimes(known),
            test_sink(&bus),
        )
        .unwrap();

        let creates = timeout(Duration::from_secs(5), async {
            loop {
                let creates: Vec<_> = bus
                    .snapshot_since(0)
                    .into_iter()
                    .filter(|e| e.type_ == "watch.create")
                    .collect();
                if creates.len() >= 2 {
                    return creates;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("expected creates did not appear");
        // Settle briefly: a late (wrong) emit for same.html would land now.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let paths: Vec<String> = bus
            .snapshot_since(0)
            .into_iter()
            .filter(|e| e.type_ == "watch.create")
            .map(|e| e.payload["path"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            paths.len(),
            2,
            "exactly the mismatched + unknown files emit: {paths:?}"
        );
        assert!(paths.iter().any(|p| p.ends_with("touched.html")));
        assert!(paths.iter().any(|p| p.ends_with("unknown.html")));
        assert!(
            !paths.iter().any(|p| p.ends_with("same.html")),
            "an unchanged already-indexed file must not re-emit on restart"
        );
        drop(creates);
    }

    /// X2/D6 — a paused source's watcher is quiet: no initial walk, no live
    /// events; resuming (a live gate flip) lets subsequent events flow.
    #[tokio::test]
    async fn paused_source_watches_silently_until_resumed() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("preexisting.html"), "<html></html>").unwrap();

        let bus = Arc::new(EventBus::default());
        let gate = crate::exclusions::IngestGate::default();
        gate.set_paused(true);
        let _w = Watcher::start(
            WatcherConfig::new(kb(), vec![tmp.path().to_path_buf()])
                .with_debounce(Duration::from_millis(50)),
            test_sink(&bus).with_gate(gate.clone()),
        )
        .unwrap();

        // While paused: neither the initial walk nor a live create emits.
        fs::write(tmp.path().join("while-paused.html"), "<html></html>").unwrap();
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(
            bus.snapshot_since(0)
                .iter()
                .all(|e| !e.type_.starts_with("watch.")),
            "a paused source must emit no watch.* at all"
        );

        // Resume (live flip — same Arc the drain thread reads) → new events flow.
        gate.set_paused(false);
        fs::write(tmp.path().join("after-resume.html"), "<html></html>").unwrap();
        let ok = timeout(Duration::from_secs(5), async {
            loop {
                if bus.snapshot_since(0).iter().any(|e| {
                    e.type_ == "watch.create"
                        && e.payload["path"]
                            .as_str()
                            .unwrap_or("")
                            .ends_with("after-resume.html")
                }) {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or(false);
        assert!(ok, "after resume, live events must flow again");
    }

    /// X2 — live events for an excluded path are dropped at `emit_for_paths`.
    #[tokio::test]
    async fn live_event_for_excluded_file_is_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let bus = Arc::new(EventBus::default());
        let gate = crate::exclusions::IngestGate::default();
        gate.add_excluded("drop.html");
        let _w = Watcher::start(
            WatcherConfig::new(kb(), vec![tmp.path().to_path_buf()])
                .with_debounce(Duration::from_millis(50)),
            test_sink(&bus).with_gate(gate),
        )
        .unwrap();

        // The excluded file first, then the keeper: both land in the same
        // debounce flush, so if the excluded one were going to emit it would
        // be present by the time the keeper's create appears.
        fs::write(tmp.path().join("drop.html"), "<html></html>").unwrap();
        fs::write(tmp.path().join("keep.html"), "<html></html>").unwrap();

        let creates = timeout(Duration::from_secs(5), async {
            loop {
                let creates: Vec<_> = bus
                    .snapshot_since(0)
                    .into_iter()
                    .filter(|e| e.type_ == "watch.create")
                    .collect();
                if creates.iter().any(|e| {
                    e.payload["path"]
                        .as_str()
                        .unwrap_or("")
                        .ends_with("keep.html")
                }) {
                    return creates;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("keeper create did not appear");
        assert!(
            !creates.iter().any(|e| e.payload["path"]
                .as_str()
                .unwrap_or("")
                .ends_with("drop.html")),
            "the excluded file's live event must be dropped"
        );
    }

    #[tokio::test]
    async fn start_tolerates_missing_source_dir() {
        // A2 — adding a kb that points at a folder which doesn't exist yet must
        // NOT fail the daemon boot. arm_debouncer used to propagate notify's
        // ENOENT, which rolled back EVERY corpus. Now it skip-and-warns: the
        // watcher arms, the kb boots empty.
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("does-not-exist-yet");
        assert!(!missing.exists());

        let bus = Arc::new(EventBus::default());
        let started = Watcher::start(
            WatcherConfig::new(kb(), vec![missing]).with_debounce(Duration::from_millis(50)),
            test_sink(&bus),
        );
        assert!(
            started.is_ok(),
            "watcher must boot (skip-and-warn) on a missing source dir, not error",
        );
    }

    #[tokio::test]
    async fn create_event_after_start_is_published() {
        let tmp = tempfile::tempdir().unwrap();
        let bus = Arc::new(EventBus::default());
        let mut rx = bus.subscribe();

        let _w = Watcher::start(
            WatcherConfig::new(kb(), vec![tmp.path().to_path_buf()])
                .with_debounce(Duration::from_millis(50)),
            test_sink(&bus),
        )
        .unwrap();

        fs::write(tmp.path().join("new.html"), "<html></html>").unwrap();

        let env = next_typed(&mut rx, "watch.create")
            .await
            .expect("no create");
        let path = env.payload["path"].as_str().unwrap();
        assert!(path.ends_with("new.html"));
        assert_eq!(env.payload["kb"].as_str().unwrap(), "smoke");
    }

    #[tokio::test]
    async fn modify_event_after_start_is_published() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("file.html");
        fs::write(&p, "<html>v1</html>").unwrap();

        let bus = Arc::new(EventBus::default());
        let _w = Watcher::start(
            WatcherConfig::new(kb(), vec![tmp.path().to_path_buf()])
                .with_debounce(Duration::from_millis(50)),
            test_sink(&bus),
        )
        .unwrap();

        // Drain the initial-walk create.
        let mut rx = bus.subscribe();

        // Sleep a beat so the watcher's inotify subscription is live before
        // we mutate. (notify's setup is async-ish on Linux.)
        tokio::time::sleep(Duration::from_millis(150)).await;
        fs::write(&p, "<html>v2</html>").unwrap();

        let env = next_typed(&mut rx, "watch.modify")
            .await
            .expect("no modify");
        let path = env.payload["path"].as_str().unwrap();
        assert!(path.ends_with("file.html"));
    }

    // The Poll backend (`watch_mode = "poll"`, `HeldDebouncer::Poll`) is
    // otherwise only compile-checked. Drive a real `notify::PollWatcher` with a
    // short re-stat interval and assert a live CREATE is published end-to-end —
    // this is the backend WSL `/mnt/*` + SMB/NFS users actually run, and a new
    // file is the change `PollWatcher` detects most deterministically (a
    // metadata-only modify depends on mtime granularity). Polling is
    // backend-agnostic. The reconcile pass is the correctness backstop
    // regardless of live delivery.
    #[tokio::test]
    async fn poll_backend_publishes_live_create() {
        let tmp = tempfile::tempdir().unwrap();
        // One pre-existing file so the watcher's first scan has a baseline.
        fs::write(tmp.path().join("seed.html"), "<html>seed</html>").unwrap();

        let bus = Arc::new(EventBus::default());
        let _w = Watcher::start(
            WatcherConfig::new(kb(), vec![tmp.path().to_path_buf()])
                .with_watch_mode(WatchMode::Poll)
                .with_poll_interval(Duration::from_millis(100))
                .with_debounce(Duration::from_millis(50)),
            test_sink(&bus),
        )
        .unwrap();

        let mut rx = bus.subscribe();
        // Let the poll watcher establish its baseline scan before adding a file.
        tokio::time::sleep(Duration::from_millis(300)).await;
        fs::write(tmp.path().join("late.html"), "<html>added</html>").unwrap();

        // Wait specifically for the LIVE create of late.html — ignore the
        // initial-walk create of seed.html that may still be draining.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut saw_late = false;
        while Instant::now() < deadline {
            match timeout(Duration::from_millis(200), rx.recv()).await {
                Ok(Ok(env))
                    if env.type_ == "watch.create"
                        && env.payload["path"]
                            .as_str()
                            .is_some_and(|p| p.ends_with("late.html")) =>
                {
                    saw_late = true;
                    break;
                }
                Ok(_) => continue,
                Err(_) => continue,
            }
        }
        assert!(
            saw_late,
            "poll backend did not deliver a live create for late.html"
        );
    }

    #[tokio::test]
    async fn delete_event_after_start_is_published() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("doomed.html");
        fs::write(&p, "<html></html>").unwrap();

        let bus = Arc::new(EventBus::default());
        let _w = Watcher::start(
            WatcherConfig::new(kb(), vec![tmp.path().to_path_buf()])
                .with_debounce(Duration::from_millis(50)),
            test_sink(&bus),
        )
        .unwrap();

        let mut rx = bus.subscribe();
        tokio::time::sleep(Duration::from_millis(150)).await;
        fs::remove_file(&p).unwrap();

        let env = next_typed(&mut rx, "watch.delete")
            .await
            .expect("no delete");
        let path = env.payload["path"].as_str().unwrap();
        assert!(path.ends_with("doomed.html"));
    }

    #[tokio::test]
    async fn non_html_files_are_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let bus = Arc::new(EventBus::default());
        let mut rx = bus.subscribe();

        let _w = Watcher::start(
            WatcherConfig::new(kb(), vec![tmp.path().to_path_buf()])
                .with_debounce(Duration::from_millis(50)),
            test_sink(&bus),
        )
        .unwrap();

        tokio::time::sleep(Duration::from_millis(150)).await;
        fs::write(tmp.path().join("notes.txt"), "hi").unwrap();

        // Should NOT see a watch.create for the .txt file.
        let result = timeout(Duration::from_millis(800), async {
            loop {
                match rx.recv().await {
                    Ok(env) if env.type_ == "watch.create" => return Some(env),
                    Ok(_) => continue,
                    Err(_) => return None,
                }
            }
        })
        .await;
        assert!(
            result.is_err() || result.unwrap().is_none(),
            ".txt file should not produce watch.create"
        );
    }

    #[test]
    fn ingest_gate_recognises_html_and_markdown() {
        use crate::indexer::is_indexable;
        assert!(is_indexable(Path::new("/x/y.html")));
        assert!(is_indexable(Path::new("/x/y.HTML")));
        assert!(is_indexable(Path::new("/x/y.htm")));
        assert!(is_indexable(Path::new("/x/y.md")));
        assert!(is_indexable(Path::new("/x/y.markdown")));
        assert!(!is_indexable(Path::new("/x/y.txt")));
        assert!(!is_indexable(Path::new("/x/y")));
    }

    #[test]
    fn is_config_file_recognises() {
        assert!(is_config_file(Path::new("/etc/kb/kb.toml")));
        assert!(is_config_file(Path::new("/x/daemons.toml")));
        assert!(!is_config_file(Path::new("/x/random.toml")));
    }

    #[test]
    fn skip_pattern_prefix_glob() {
        assert!(path_matches_skip_pattern(
            "templates/report-daily.html",
            "report-daily.html",
            "templates/**"
        ));
        assert!(path_matches_skip_pattern(
            "templates",
            "templates",
            "templates/**"
        ));
        assert!(!path_matches_skip_pattern(
            "ideas/templates/sample.html",
            "sample.html",
            "templates/**"
        ));
    }

    #[test]
    fn skip_pattern_recursive_middle() {
        assert!(!path_matches_skip_pattern(
            "features/archive/legacy/x.html",
            "x.html",
            "**/.git/**"
        ));
        assert!(path_matches_skip_pattern(
            "a/.git/HEAD",
            "HEAD",
            "**/.git/**"
        ));
        assert!(path_matches_skip_pattern(".git/foo", "foo", "**/.git/**"));
        assert!(path_matches_skip_pattern(
            "nested/deep/.git/bar",
            "bar",
            "**/.git/**"
        ));
    }

    #[test]
    fn skip_pattern_leaf_basename() {
        assert!(path_matches_skip_pattern(
            "foo/.DS_Store",
            ".DS_Store",
            "**/.DS_Store"
        ));
        assert!(path_matches_skip_pattern(
            ".DS_Store",
            ".DS_Store",
            "**/.DS_Store"
        ));
        assert!(!path_matches_skip_pattern(
            "foo/regular.html",
            "regular.html",
            "**/.DS_Store"
        ));
    }

    #[test]
    fn skip_pattern_extension_glob() {
        assert!(path_matches_skip_pattern(
            "scratch/tmp.tmp",
            "tmp.tmp",
            "*.tmp"
        ));
        assert!(path_matches_skip_pattern("a.tmp", "a.tmp", "*.tmp"));
        assert!(!path_matches_skip_pattern(
            "real.html",
            "real.html",
            "*.tmp"
        ));
        // v0.7.1 P2 — a file literally named `.tmp` (empty stem) matches
        // `*.tmp`; a file named `tmp` (no extension) does not.
        assert!(path_matches_skip_pattern("scratch/.tmp", ".tmp", "*.tmp"));
        assert!(!path_matches_skip_pattern("scratch/tmp", "tmp", "*.tmp"));
    }

    #[test]
    fn skip_pattern_literal_name() {
        // No wildcards: matches whole rel path OR basename.
        assert!(path_matches_skip_pattern(".git", ".git", ".git"));
        assert!(path_matches_skip_pattern("project/.git", ".git", ".git"));
        assert!(!path_matches_skip_pattern("git", "git", ".git"));
    }

    #[test]
    fn skip_pattern_empty_returns_false() {
        assert!(!path_matches_skip_pattern("a/b", "b", ""));
        assert!(!path_matches_skip_pattern("a/b", "b", "   "));
    }

    #[test]
    fn is_skipped_combines_patterns() {
        let base = Path::new("/kb");
        let templates_file = Path::new("/kb/templates/report-daily.html");
        let normal_file = Path::new("/kb/ideas/foo.html");
        let patterns = vec!["templates/**".to_string(), "**/.git/**".to_string()];
        assert!(is_skipped(templates_file, base, &patterns));
        assert!(!is_skipped(normal_file, base, &patterns));
    }

    #[test]
    fn is_skipped_empty_list_passes_everything() {
        let base = Path::new("/kb");
        let path = Path::new("/kb/whatever.html");
        assert!(!is_skipped(path, base, &[]));
    }

    #[tokio::test]
    async fn initial_walk_honours_skip_patterns() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("templates")).unwrap();
        fs::create_dir_all(tmp.path().join("ideas")).unwrap();
        fs::write(tmp.path().join("templates/skel.html"), "<html></html>").unwrap();
        fs::write(tmp.path().join("ideas/real.html"), "<html></html>").unwrap();

        let bus = Arc::new(EventBus::default());
        let _w = Watcher::start(
            WatcherConfig::new(kb(), vec![tmp.path().to_path_buf()])
                .with_debounce(Duration::from_millis(50))
                .with_skip_patterns(vec!["templates/**".to_string()]),
            test_sink(&bus),
        )
        .unwrap();

        // Initial walk runs on the drain thread now; poll until it settles.
        let creates = timeout(Duration::from_secs(5), async {
            loop {
                let creates: Vec<_> = bus
                    .snapshot_since(0)
                    .into_iter()
                    .filter(|e| e.type_ == "watch.create")
                    .collect();
                // Wait for at least one; templates/ must stay excluded.
                if !creates.is_empty() {
                    return creates;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("initial-walk create did not appear");
        assert_eq!(
            creates.len(),
            1,
            "templates/ skipped → 1 create for ideas/real.html"
        );
        let path = creates[0].payload["path"].as_str().unwrap();
        assert!(path.ends_with("ideas/real.html"));
    }
}
