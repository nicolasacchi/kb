//! `tracing` initialisation for the daemon entry points (the `kb-server`
//! binary and the in-process `kb daemon` path). Layered registry:
//!
//! - **stderr** — human-readable, `RUST_LOG` else the caller-supplied
//!   default directives. Callers pass their pre-existing defaults so
//!   `docker logs` / journald output is byte-identical to the old
//!   per-binary init.
//! - **file** — ndjson (one JSON object per line), daily-rolled
//!   `<state>/log/kb.ndjson.YYYY-MM-DD`. Defaults to `info` (an unbounded
//!   `debug` file is a disk-fill risk at daemon traffic rates); override
//!   with [`FILE_FILTER_ENV`], which accepts full `EnvFilter` directives
//!   (`debug`, `info,kb_core=debug`, …).
//!
//! Returns the appender's `WorkerGuard` — the daemon must keep it alive for
//! the lifetime of the process or buffered logs may be dropped on exit.

use crate::paths::KbPaths;
use crate::{Error, Result};
use std::path::Path;
use std::sync::OnceLock;
use std::time::SystemTime;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::layer::Layer;
use tracing_subscriber::{
    fmt, layer::SubscriberExt, reload, util::SubscriberInitExt, EnvFilter, Registry,
};

/// Env var holding `EnvFilter` directives for the FILE layer only. The
/// stderr layer stays governed by `RUST_LOG` + the caller's default.
pub const FILE_FILTER_ENV: &str = "KB_LOG_FILE_LEVEL";

/// Rolling-appender file-name prefix. The daily appender writes
/// `<state>/log/<PREFIX>.YYYY-MM-DD`; the L2 retention sweep
/// ([`prune_logs_older_than`]) deletes ONLY files carrying this prefix so
/// it can never touch a foreign file an operator parked in the log dir.
pub const LOG_FILE_PREFIX: &str = "kb.ndjson";

/// File-layer default: `info` globally, with the lance scanner pinned to
/// `error` — lance 4.0.0 emits a paired `_score`/`_distance`
/// auto-projection deprecation WARN on every FTS + vector search, the
/// same noise both binaries already pin out of their stderr defaults.
const DEFAULT_FILE_FILTER: &str = "info,lance::dataset::scanner=error";

/// L2 — reload handle for the FILE layer's `EnvFilter`, letting
/// `PUT /api/log-level` flip verbosity without a restart. The handle type
/// is pinned to the concrete subscriber [`init`] builds (`Registry` is the
/// base of the layered stack), mirroring the `install_corpus_mounts`
/// process-global precedent. `None` until [`init`] runs (or a test
/// installs one) — the daemon entry points call `init` exactly once, and
/// an in-process config-reload restart does NOT re-init tracing, so
/// set-once semantics hold for the process lifetime.
pub type FileFilterHandle = reload::Handle<EnvFilter, Registry>;
static FILE_FILTER_HANDLE: OnceLock<FileFilterHandle> = OnceLock::new();

/// Install the file-layer reload handle process-globally. Returns `false`
/// (leaving the existing handle in place) when one is already installed.
/// Public so tests — including kb-server's route tests, which never call
/// [`init`] (a global subscriber would poison every other test) — can
/// install a handle scoped to their own layer stack.
pub fn install_file_filter_handle(handle: FileFilterHandle) -> bool {
    FILE_FILTER_HANDLE.set(handle).is_ok()
}

/// Current FILE-layer filter directives, `None` when file logging was
/// never initialised in this process (or its layer is gone).
pub fn current_file_filter() -> Option<String> {
    let handle = FILE_FILTER_HANDLE.get()?;
    handle.with_current(|f| f.to_string()).ok()
}

/// L2 — flip the FILE layer's `EnvFilter` at runtime. Validates the
/// directives FIRST (bad input → `BadRequest`, whether or not logging is
/// initialised), then reloads through the installed handle
/// (`Conflict` when no handle — e.g. an embedded daemon that never ran
/// [`init`]). Returns the filter string now in effect. The stderr layer
/// (RUST_LOG) is deliberately untouched — `docker logs`/journald output
/// stays stable while the operator turns the file up to `debug`.
pub fn set_file_filter(directives: &str) -> Result<String> {
    let trimmed = directives.trim();
    if trimmed.is_empty() {
        return Err(Error::BadRequest(
            "log filter is empty — pass EnvFilter directives, e.g. `debug` or \
             `info,kb_core=debug`"
                .into(),
        ));
    }
    let filter = EnvFilter::try_new(trimmed)
        .map_err(|e| Error::BadRequest(format!("invalid log filter `{trimmed}`: {e}")))?;
    let handle = FILE_FILTER_HANDLE.get().ok_or_else(|| {
        Error::Conflict(
            "file logging is not initialised in this daemon process, so there is no \
             filter to change (the `kb daemon` / kb-server entry points initialise it)"
                .into(),
        )
    })?;
    handle
        .reload(filter)
        .map_err(|e| Error::Internal(anyhow::anyhow!("reload file log filter: {e}")))?;
    Ok(current_file_filter().unwrap_or_else(|| trimmed.to_string()))
}

/// Initialise the global tracing subscriber: stderr layer (RUST_LOG else
/// `stderr_default`) + ndjson daily file layer under `paths.log`.
/// Panics if a global subscriber is already set (the standard
/// `set_global_default` convention) — call once, from `main`.
pub fn init(paths: &KbPaths, stderr_default: &str) -> Result<WorkerGuard> {
    let (file_layer, reload_handle, guard) = file_layer(paths, file_filter_from_env())?;

    let stderr_layer = fmt::layer().with_writer(std::io::stderr).with_filter(
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(stderr_default)),
    );

    tracing_subscriber::registry()
        .with(file_layer)
        .with(stderr_layer.boxed())
        .init();

    // L2 — stash the file-layer reload handle so PUT /api/log-level can
    // flip verbosity live. After `.init()` so a failed init never leaves
    // a handle pointing at a subscriber that was never installed.
    install_file_filter_handle(reload_handle);

    Ok(guard)
}

/// Build the ndjson file layer + its reload handle + flush guard. Split
/// out (and generic over the subscriber) so tests can compose it into a
/// scoped `with_default` subscriber without touching the global
/// dispatcher. The `EnvFilter` is wrapped in a `reload::Layer` so the
/// returned handle can swap it live (L2 runtime log level); the handle
/// holds a `Weak` ref — it goes stale if the layer drops.
#[allow(clippy::type_complexity)]
fn file_layer<S>(
    paths: &KbPaths,
    filter: EnvFilter,
) -> Result<(
    Box<dyn Layer<S> + Send + Sync + 'static>,
    reload::Handle<EnvFilter, S>,
    WorkerGuard,
)>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    paths.ensure_dirs()?;

    let appender = tracing_appender::rolling::daily(&paths.log, LOG_FILE_PREFIX);
    let (non_blocking, guard) = tracing_appender::non_blocking(appender);

    let (filter, handle) = reload::Layer::new(filter);
    let layer = fmt::layer()
        .json()
        .with_writer(non_blocking)
        .with_filter(filter)
        .boxed();
    Ok((layer, handle, guard))
}

/// L2 — delete aged daemon log files: regular files in `dir` whose name
/// starts with [`LOG_FILE_PREFIX`] and whose mtime is strictly older than
/// `cutoff`. Returns the number removed. A missing dir is `Ok(0)` (fresh
/// daemon, nothing to prune); per-file removal errors are logged and
/// skipped so one stubborn file can't wedge the sweep. Pure-ish (caller
/// supplies the cutoff) so the age policy is unit-testable without clock
/// mocking. Prune-not-edit, in the spirit of invariant #8's retention
/// exception: whole aged files are deleted, never rewritten.
pub fn prune_logs_older_than(dir: &Path, cutoff: SystemTime) -> Result<usize> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e.into()),
    };
    let mut removed = 0;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with(LOG_FILE_PREFIX) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        // No mtime on this platform/fs → keep the file (never delete on
        // missing evidence). Today's active file always has a fresh mtime,
        // so a ≥1-day window can never reap the file being written.
        let Ok(modified) = meta.modified() else {
            continue;
        };
        if modified >= cutoff {
            continue;
        }
        match std::fs::remove_file(entry.path()) {
            Ok(()) => removed += 1,
            Err(e) => tracing::warn!(
                file = %entry.path().display(),
                error = %e,
                "log retention: failed to remove aged log file"
            ),
        }
    }
    Ok(removed)
}

fn file_filter_from_env() -> EnvFilter {
    resolve_file_filter(std::env::var(FILE_FILTER_ENV).ok().as_deref())
}

/// `EnvFilter` for the file layer: the env value when set + non-blank,
/// else [`DEFAULT_FILE_FILTER`]. Pure so the precedence is unit-testable
/// without racing on process-global env vars.
fn resolve_file_filter(env_val: Option<&str>) -> EnvFilter {
    match env_val {
        Some(v) if !v.trim().is_empty() => EnvFilter::new(v),
        _ => EnvFilter::new(DEFAULT_FILE_FILTER),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing::level_filters::LevelFilter;

    #[test]
    fn file_filter_defaults_to_info() {
        assert_eq!(
            resolve_file_filter(None).max_level_hint(),
            Some(LevelFilter::INFO)
        );
        // Blank / whitespace-only env values fall back to the default too.
        assert_eq!(
            resolve_file_filter(Some("")).max_level_hint(),
            Some(LevelFilter::INFO)
        );
        assert_eq!(
            resolve_file_filter(Some("   ")).max_level_hint(),
            Some(LevelFilter::INFO)
        );
    }

    #[test]
    fn file_filter_env_override_wins() {
        assert_eq!(
            resolve_file_filter(Some("debug")).max_level_hint(),
            Some(LevelFilter::DEBUG)
        );
        // Full directive strings are accepted, not just bare levels.
        assert_eq!(
            resolve_file_filter(Some("warn,kb_core=trace")).max_level_hint(),
            Some(LevelFilter::TRACE)
        );
    }

    /// The file layer writes ndjson (one parseable JSON object per line)
    /// and gates on the supplied filter: info passes, debug is dropped.
    /// Scoped via `with_default` so the global dispatcher is untouched.
    #[test]
    fn file_layer_writes_ndjson_and_gates_below_info() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = KbPaths::rooted_at(tmp.path(), "smoke");

        let (layer, _handle, guard) =
            file_layer::<tracing_subscriber::Registry>(&paths, EnvFilter::new("info")).unwrap();
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(marker = "l1_keep", "info-level line");
            tracing::debug!(marker = "l1_drop", "debug-level line");
        });
        drop(guard); // flush the non-blocking writer

        let mut body = String::new();
        for entry in std::fs::read_dir(&paths.log).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            assert!(
                name.starts_with("kb.ndjson"),
                "unexpected file in log dir: {name}"
            );
            body.push_str(&std::fs::read_to_string(&path).unwrap());
        }

        assert!(
            body.contains("l1_keep"),
            "info-level event missing from file: {body}"
        );
        assert!(
            !body.contains("l1_drop"),
            "debug-level event leaked past the info filter: {body}"
        );
        for line in body.lines().filter(|l| !l.trim().is_empty()) {
            let v: serde_json::Value =
                serde_json::from_str(line).unwrap_or_else(|e| panic!("not ndjson ({e}): {line}"));
            assert!(v.get("level").is_some(), "no level field: {line}");
            assert!(v.get("timestamp").is_some(), "no timestamp field: {line}");
        }
    }

    /// Concatenate every log file the appender wrote under `paths.log`.
    fn read_log_dir(paths: &KbPaths) -> String {
        let mut body = String::new();
        for entry in std::fs::read_dir(&paths.log).unwrap() {
            body.push_str(&std::fs::read_to_string(entry.unwrap().path()).unwrap());
        }
        body
    }

    /// L2 — reloading the file layer's filter changes gating LIVE: a debug
    /// event is dropped at `info`, then passes after `handle.reload(debug)`
    /// — same subscriber, no re-init. This pins the mechanism
    /// `PUT /api/log-level` rides.
    #[test]
    fn file_layer_reload_flips_gating_live() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = KbPaths::rooted_at(tmp.path(), "reload");

        let (layer, handle, guard) =
            file_layer::<tracing_subscriber::Registry>(&paths, EnvFilter::new("info")).unwrap();
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::debug!(marker = "l2_before_flip", "dropped at info");
            handle.reload(EnvFilter::new("debug")).unwrap();
            tracing::debug!(marker = "l2_after_flip", "kept at debug");
        });
        drop(guard); // flush

        let body = read_log_dir(&paths);
        assert!(
            !body.contains("l2_before_flip"),
            "debug event leaked before the reload: {body}"
        );
        assert!(
            body.contains("l2_after_flip"),
            "debug event missing after reloading to debug: {body}"
        );
    }

    /// L2 — the process-global handle API end to end. ONE test owns the
    /// global (`OnceLock` — a second installer in this binary would race):
    /// pre-install reads are `None` / flips are 409-Conflict, bad
    /// directives are 400 regardless, a real install makes set+get
    /// roundtrip, and a second install is a refused no-op.
    #[test]
    fn global_file_filter_install_set_get_contract() {
        // Before install: nothing to read, flip refuses with Conflict
        // (409) — the route maps this straight to problem+json.
        assert!(current_file_filter().is_none());
        assert_eq!(set_file_filter("debug").unwrap_err().http_status(), 409);
        // Bad directives fail validation (400) BEFORE the handle lookup,
        // so the error is deterministic whether or not logging is up.
        assert_eq!(set_file_filter("foo=bar").unwrap_err().http_status(), 400);
        assert_eq!(set_file_filter("   ").unwrap_err().http_status(), 400);

        // Install a handle. `_layer` stays alive to the end of the test —
        // the handle holds a Weak ref and goes stale if the layer drops.
        let (_layer, handle) = reload::Layer::<EnvFilter, Registry>::new(EnvFilter::new("info"));
        assert!(install_file_filter_handle(handle));

        let now = set_file_filter("debug,kb_core=trace").unwrap();
        assert!(
            now.contains("debug"),
            "reloaded filter missing level: {now}"
        );
        assert_eq!(current_file_filter().as_deref(), Some(now.as_str()));
        // Invalid directives still refuse AND leave the filter untouched.
        assert_eq!(set_file_filter("foo=bar").unwrap_err().http_status(), 400);
        assert_eq!(current_file_filter().as_deref(), Some(now.as_str()));

        // Second install refused; the original handle keeps working.
        let (_l2, h2) = reload::Layer::<EnvFilter, Registry>::new(EnvFilter::new("warn"));
        assert!(!install_file_filter_handle(h2));
        assert!(set_file_filter("info").is_ok());
    }

    /// L2 — the retention sweep deletes ONLY aged `kb.ndjson*` files:
    /// fresh log files and foreign files (any mtime) survive, and a
    /// missing dir is a clean no-op.
    #[test]
    fn prune_removes_only_aged_log_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("log");
        std::fs::create_dir_all(&dir).unwrap();

        let aged = dir.join("kb.ndjson.2020-01-01");
        let fresh = dir.join("kb.ndjson.2026-07-12");
        let foreign = dir.join("operator-notes.txt");
        for p in [&aged, &fresh, &foreign] {
            std::fs::write(p, b"{}\n").unwrap();
        }
        // Age the doomed file AND the foreign file far past any window.
        let past = SystemTime::now() - std::time::Duration::from_secs(90 * 86_400);
        for p in [&aged, &foreign] {
            let f = std::fs::OpenOptions::new().write(true).open(p).unwrap();
            f.set_times(std::fs::FileTimes::new().set_modified(past))
                .unwrap();
        }

        let cutoff = SystemTime::now() - std::time::Duration::from_secs(14 * 86_400);
        assert_eq!(prune_logs_older_than(&dir, cutoff).unwrap(), 1);
        assert!(!aged.exists(), "aged log file should be pruned");
        assert!(fresh.exists(), "fresh log file must survive");
        assert!(foreign.exists(), "non-kb.ndjson files are never touched");

        // Idempotent + missing-dir no-op.
        assert_eq!(prune_logs_older_than(&dir, cutoff).unwrap(), 0);
        assert_eq!(
            prune_logs_older_than(&tmp.path().join("nope"), cutoff).unwrap(),
            0
        );
    }
}
