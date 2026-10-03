//! Consistent snapshot helper for `kb backup` (B1).
//!
//! The pre-B1 backup tar'd the live `index.db` (plus its `-wal`/`-shm`
//! sidecars) while the daemon could be mid-write — capturing a torn,
//! half-applied transaction. `VACUUM INTO` instead reads a
//! transactionally-consistent view under WAL and writes a fresh,
//! defragmented, standalone database file (no WAL/SHM to reconcile), so a
//! snapshot taken while the daemon writes is never torn. It needs no
//! cooperation from the running daemon.
//!
//! [`write_kb_export`] packs that snapshot into
//! `<exports>/<kb>-YYYYMMDD-HHMMSS.tar.gz` for the daemon's
//! `[backup] schedule_hours` task. It invokes `tar` directly and must not
//! exec the `kb` binary.

use crate::config::BackupSection;
use crate::paths::{KbPaths, Snapshot, StateClass, StateMember, StateScope};
use crate::types::KbName;
use crate::Result;
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

/// Write a transactionally-consistent copy of the sqlite database at
/// `src` to `dest` via `VACUUM INTO`.
///
/// Opens a fresh connection and does NOT run migrations (that would
/// mutate the live DB). `dest` must not already exist — sqlite refuses to
/// overwrite an existing file. The destination path is escaped into the
/// statement as a single-quoted literal (`VACUUM INTO` predates bound
/// parameters on some sqlite builds); callers pass a controlled staging
/// path, but the escape keeps a stray apostrophe from breaking the SQL.
pub fn vacuum_into(src: &Path, dest: &Path) -> Result<()> {
    let dest_str = dest.to_str().ok_or_else(|| {
        crate::Error::Storage(format!("backup: non-utf8 destination {}", dest.display()))
    })?;
    let conn = Connection::open(src)
        .map_err(|e| crate::Error::Storage(format!("backup: open {}: {e}", src.display())))?;
    let sql = format!("VACUUM INTO '{}'", dest_str.replace('\'', "''"));
    conn.execute(&sql, [])
        .map_err(|e| crate::Error::Storage(format!("backup: VACUUM INTO {dest_str}: {e}")))?;
    Ok(())
}

/// Outcome of an attempted GC-B4 off-host copy. `run_remote_copy` always
/// returns `None` when `[backup]` isn't fully configured — callers can
/// treat `None` as "nothing to report".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteCopyOutcome {
    /// The command exited 0.
    Ok,
    /// The command ran and exited non-zero, or failed to spawn at all.
    /// `message` is loggable/printable as-is (never contains the
    /// destination's credentials — those live in `remote_dest`/argv,
    /// which the operator controls and is responsible for scrubbing from
    /// their own logs if sensitive).
    Failed { message: String },
}

/// How long [`run_remote_copy`] waits, per pipe, for its output drain to finish
/// after the uploader has exited.
const REMOTE_COPY_DRAIN_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// Best-effort off-host copy of a completed local backup (GC-B4).
///
/// Runs `cfg.remote_cmd` (argv, `{src}`/`{dest}` substituted) via
/// `std::process::Command` — no shell, so no quoting/injection surface.
/// Returns `None` when `[backup]` isn't fully configured (the common
/// case — the feature is opt-in). A failure to spawn or a non-zero exit
/// is reported as `RemoteCopyOutcome::Failed`, never as an `Err` —
/// **the remote copy is best-effort and must never fail the backup
/// itself**; callers surface the outcome loudly (log + CLI exit
/// message) but keep the backup's own success.
///
/// The uploader runs in its OWN process group with a wall deadline
/// ([`BackupSection::remote_timeout`]). At the deadline the whole group
/// is killed and the copy is reported `Failed { "timed out" }`: an
/// `scp` stalled on a half-open connection used to block the schedule
/// task, and so every later kb and tick, forever.
pub fn run_remote_copy(cfg: &BackupSection, src: &Path) -> Option<RemoteCopyOutcome> {
    use std::io::Read;
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    let argv = cfg.build_argv(src)?;
    // `is_configured`/`build_argv` guarantee argv is non-empty.
    let (program, args) = argv
        .split_first()
        .expect("build_argv yields non-empty argv");
    let spawned = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn();
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => {
            let message = format!("failed to spawn `{program}`: {e}");
            tracing::warn!(program, %message, "backup: off-host copy FAILED");
            return Some(RemoteCopyOutcome::Failed { message });
        }
    };
    let pgid = child.id() as libc::pid_t;
    let drain = |pipe: Option<Box<dyn std::io::Read + Send>>| {
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut kept = Vec::new();
            if let Some(mut pipe) = pipe {
                let mut buf = [0u8; 8192];
                while let Ok(n) = pipe.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    // Keep draining past the cap so the child never
                    // blocks on a full pipe.
                    if kept.len() < 64 * 1024 {
                        kept.extend_from_slice(&buf[..n]);
                    }
                }
            }
            let _ = tx.send(kept);
        });
        rx
    };
    let out_t = drain(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
    );
    let err_t = drain(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
    );
    let deadline = std::time::Instant::now() + cfg.remote_timeout();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Ok(st),
            Ok(None) => {}
            Err(e) => break Err(e),
        }
        if std::time::Instant::now() >= deadline {
            timed_out = true;
            // SAFETY: killpg on the group this function created with
            // `process_group(0)`; the pgid is the child's own pid.
            unsafe {
                libc::killpg(pgid, libc::SIGKILL);
            }
            break child.wait();
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    // Normally the group is dead (or exited), so the pipes close and the
    // drains end at once. A child that daemonised and kept the pipes open
    // would hold a plain `join` forever (and with it the scheduler), so the
    // wait is bounded; a straggling drain thread is left to die with its pipe.
    let _ = out_t.recv_timeout(REMOTE_COPY_DRAIN_GRACE);
    let stderr_bytes = err_t
        .recv_timeout(REMOTE_COPY_DRAIN_GRACE)
        .unwrap_or_default();
    Some(match status {
        _ if timed_out => {
            let message = format!(
                "`{program}` timed out after {}s and was killed",
                cfg.remote_timeout().as_secs()
            );
            tracing::warn!(program, %message, "backup: off-host copy FAILED");
            RemoteCopyOutcome::Failed { message }
        }
        Ok(st) if st.success() => {
            tracing::info!(program, dest = %cfg.remote_dest.as_deref().unwrap_or(""), "backup: off-host copy succeeded");
            RemoteCopyOutcome::Ok
        }
        Ok(st) => {
            let stderr = String::from_utf8_lossy(&stderr_bytes);
            let message = format!(
                "`{program}` exited {st}: {}",
                stderr.trim().lines().next().unwrap_or("(no stderr)")
            );
            tracing::warn!(program, %message, "backup: off-host copy FAILED");
            RemoteCopyOutcome::Failed { message }
        }
        Err(e) => {
            let message = format!("failed to wait for `{program}`: {e}");
            tracing::warn!(program, %message, "backup: off-host copy FAILED");
            RemoteCopyOutcome::Failed { message }
        }
    })
}

/// `<tarball>.uploaded` — written only after the off-host copy of that
/// exact tarball exited 0. The scheduler's skip predicate is local-only,
/// so without this record a failed upload of a quiet kb was never
/// retried: the failed tarball was itself the "newest, nothing changed"
/// anchor.
pub fn uploaded_marker(tarball: &Path) -> PathBuf {
    sidecar(tarball, ".uploaded")
}

/// Record that `tarball` reached the off-host target.
pub fn mark_uploaded(tarball: &Path) -> Result<()> {
    std::fs::write(uploaded_marker(tarball), b"")?;
    Ok(())
}

/// Has `tarball` been copied off-host?
pub fn is_uploaded(tarball: &Path) -> bool {
    uploaded_marker(tarball).is_file()
}

/// Drop guard so a failed or cancelled export does not leave
/// `<exports>/.staging-*` behind.
struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Ceiling on directory entries one skip decision may stat, shared
/// across every packed tree of one kb.
///
/// The walk is stat-only (no re-tar, no hashing — the tarball can be
/// gigabytes), once per `[backup] schedule_hours` tick. A store that
/// blows the ceiling is a store whose lance dataset is enormous; paying
/// one extra tarball there is cheaper than guessing.
const CHANGE_PROBE_ENTRY_BUDGET: usize = 20_000;

/// What a change probe could establish about one packed tree.
enum TreeProbe {
    /// The tree is not packed for this kb (absent) — nothing to watch.
    Absent,
    /// Newest mtime found anywhere under the tree.
    Newest(SystemTime),
    /// The walk could not be completed, so "unchanged" is UNPROVEN.
    Unknown,
}

/// The ONE kb whose tarball carries the daemon-scope members (`slates/`,
/// the daemon JSON files): the first in NAME order among `kbs`, whatever
/// order the caller holds them in. The daemon schedule, `kb backup --all`
/// and `kb doctor --hooks` all choose through this, so they cannot disagree
/// about which tarball a slate append makes "changed" (`kb backup --all`
/// once took the first kb in `GET /api/kbs` response order instead).
pub fn daemon_scope_kb<'a>(kbs: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    kbs.into_iter().min()
}

/// `true` when a scheduled export of `kb` should not write a tarball.
///
/// Skip when the index is missing (nothing to snapshot), or when
/// NOTHING [`write_kb_export_with`] packs is newer than the newest
/// `<exports>/<kb>-YYYYMMDD-HHMMSS.tar.gz` — by mtime, not by name.
/// The watched set is the packed set because both iterate
/// [`KbPaths::state_members`]: `index.db` and its `-wal` sidecar, then
/// every other per-kb persistent member (`lance/`, `.review/`,
/// `.attachments/`, `.proposals/`), then — only when `include_daemon` —
/// the daemon-scope members (`slates/` and the daemon JSON files).
///
/// `include_daemon` must be true for exactly the ONE kb whose tarball
/// carries the daemon-scope members (see [`ExportOptions`]). Probing
/// them for every kb, as this function once did, made any slate append
/// "changed" for ALL kbs, so every tick wrote a full tarball of every kb.
///
/// WHY the trees count and not just the db (2026-09-30, adversarial
/// review O4): `slates/` appends JSONL directly, a comment/tag/anchor
/// edit rewrites a `.review/<id>.json` sidecar, and an attachment upload
/// writes a blob — none of them touches sqlite.
///
/// A missing exports dir or no matching tarball is not a skip: the first
/// tick must write one. Unreadable metadata is never a skip either, and
/// neither is an unfinished or over-budget probe (see
/// [`probe_tree_mtime`]) — every "I could not tell" falls through to
/// writing. `-shm` is ignored: a reader can touch it without a commit.
///
/// Synchronous stat walk of up to [`CHANGE_PROBE_ENTRY_BUDGET`] entries:
/// async callers run it on the blocking pool.
pub fn should_skip_scheduled_backup(paths: &KbPaths, kb: &KbName, include_daemon: bool) -> bool {
    skip_within_budget(paths, kb, include_daemon, CHANGE_PROBE_ENTRY_BUDGET)
}

/// [`should_skip_scheduled_backup`] with the stat budget as a parameter,
/// so the "a probe that cannot finish must write" rule is testable
/// without inventing a 20 000-entry fixture tree.
fn skip_within_budget(paths: &KbPaths, kb: &KbName, include_daemon: bool, budget: usize) -> bool {
    let index_db = paths.kb_sqlite(kb);
    if !index_db.is_file() {
        return true;
    }
    let Some(since) = newest_scheduled_tarball_mtime(&paths.exports, kb.as_str()) else {
        return false;
    };
    match index_write_mtime(&index_db) {
        Some(mtime) if mtime > since => return false,
        None => return false,
        Some(_) => {}
    }
    let mut budget = budget;
    for member in packed_members(paths, kb, include_daemon) {
        if member.name == "index.db" {
            continue;
        }
        match probe_tree_mtime(&member.path, &mut budget) {
            TreeProbe::Newest(mtime) if mtime > since => return false,
            TreeProbe::Unknown => return false,
            TreeProbe::Absent | TreeProbe::Newest(_) => {}
        }
    }
    true
}

/// The members one tarball packs: every persistent per-kb member, plus
/// the persistent daemon-scope members when `include_daemon`. The ONE
/// selection the packer and the skip probe share.
fn packed_members(paths: &KbPaths, kb: &KbName, include_daemon: bool) -> Vec<StateMember> {
    paths
        .state_members(kb)
        .into_iter()
        .filter(|m| m.is_persistent() && (m.scope == StateScope::PerKb || include_daemon))
        .collect()
}

/// Newest mtime anywhere under `root`, charging every visited entry to
/// the shared `budget`.
///
/// Directory mtimes count too, and that is not redundant: a sidecar
/// replaced by the atomic temp-file-and-rename write bumps only the
/// DIRECTORY when the replacement keeps an older stamp, and a DELETED
/// sidecar leaves no file behind at all. Watching files alone would let
/// both look unchanged and skip the export.
///
/// [`TreeProbe::Unknown`] on a walk error or an exhausted budget, which
/// the caller must read as CHANGED. That direction is the whole point:
/// a budget-triggered "unchanged" would re-open the very data-loss hole
/// this probe exists to close, while an extra tarball costs only disk the
/// operator prunes like any other export.
fn probe_tree_mtime(root: &Path, budget: &mut usize) -> TreeProbe {
    // A missing root is the common case (no lance dataset, no slates on
    // this daemon) and must not be conflated with a walk that failed.
    match std::fs::symlink_metadata(root) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return TreeProbe::Absent,
        Err(_) => return TreeProbe::Unknown,
    }
    let mut newest = None;
    for entry in walkdir::WalkDir::new(root) {
        if *budget == 0 {
            return TreeProbe::Unknown;
        }
        *budget -= 1;
        // `walkdir::DirEntry::metadata` always calls `symlink_metadata`
        // unless the walk follows links (this one does not), so a
        // dangling link inside a packed tree costs one lstat instead of
        // hanging the tick. Anything that still fails to stat is an "I
        // could not tell" → Unknown, never silently absent: a file whose
        // mtime we cannot read is a file whose change we cannot rule out.
        let Ok(entry) = entry else {
            return TreeProbe::Unknown;
        };
        let Ok(md) = entry.metadata() else {
            return TreeProbe::Unknown;
        };
        let Ok(mtime) = md.modified() else {
            return TreeProbe::Unknown;
        };
        newest = Some(match newest {
            Some(prev) if prev >= mtime => prev,
            _ => mtime,
        });
    }
    match newest {
        Some(mtime) => TreeProbe::Newest(mtime),
        None => TreeProbe::Unknown,
    }
}

/// What one export tarball carries and where it lands.
#[derive(Debug, Clone, Default)]
pub struct ExportOptions {
    /// Also pack the daemon-scope members (`slates/`, `saved-queries.json`,
    /// `memory-policy.json`, `tombstone-era.json`) beside `<kb>/`. They are
    /// daemon-wide, not per-kb, so a multi-kb sweep packs them into ONE
    /// kb's tarball (the schedule designates the first kb in name order).
    /// `kb restore` puts them back only when absent.
    pub include_daemon: bool,
    /// Explicit output path (`kb backup --out`). `None` =
    /// `<exports>/<kb>-YYYYMMDD-HHMMSS.tar.gz`, the name the skip
    /// predicate keys on.
    pub out: Option<PathBuf>,
}

/// [`write_kb_export_with`] with daemon-scope members included and the
/// default output name.
pub async fn write_kb_export(paths: &KbPaths, kb: &KbName) -> Result<PathBuf> {
    write_kb_export_with(
        paths,
        kb,
        &ExportOptions {
            include_daemon: true,
            out: None,
        },
    )
    .await
}

/// Write one restore-compatible export tarball for `kb`.
///
/// The single writer behind `kb backup`, `kb backup --all` and the
/// daemon's `[backup] schedule_hours` task. Layout: `<kb>/<member>` for
/// each persistent per-kb member of [`KbPaths::state_members`] that exists
/// (`index.db` via [`vacuum_into`], then `lance/`, `.review/`,
/// `.attachments/`, `.proposals/`), and with `include_daemon` a sibling
/// `<member>` per persistent daemon-scope member. `tar` is executed
/// directly — this function must not shell out to the `kb` binary. A
/// missing index is an error (this does not create one).
///
/// The tarball is written to `<out>.<pid>.partial` and renamed into place
/// only when `tar` succeeded, so a crash, a full disk or a kill can never
/// leave a truncated file under a name the skip predicate or `kb doctor`
/// would read as a backup. Its mtime is set to the instant the snapshot
/// STARTED, not when `tar` finished: a source edited while the tarball
/// was being built is then newer than it and the next tick packs it.
pub async fn write_kb_export_with(
    paths: &KbPaths,
    kb: &KbName,
    opts: &ExportOptions,
) -> Result<PathBuf> {
    // `Connection::open` creates a missing file. Refuse first so a scheduled
    // tick cannot invent an empty live index as a side effect of snapshotting.
    let sqlite_src = paths.kb_sqlite(kb);
    if !sqlite_src.is_file() {
        return Err(crate::Error::Storage(format!(
            "backup: kb {kb} has no index at {}",
            sqlite_src.display()
        )));
    }
    std::fs::create_dir_all(&paths.exports)?;
    let snapshot_started = SystemTime::now();
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let out = opts
        .out
        .clone()
        .unwrap_or_else(|| paths.exports.join(format!("{kb}-{stamp}.tar.gz")));
    if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let staging = paths
        .exports
        .join(format!(".staging-{kb}-{}-{stamp}", std::process::id()));
    // Fallback for a cancelled future. The normal path removes the
    // staging tree explicitly, on the blocking pool, below.
    let _cleanup = RemoveOnDrop(staging.clone());
    let result = export_inner(paths, kb, opts, &staging, &out, snapshot_started).await;
    let cleanup = staging.clone();
    // Deleting a multi-GB staged lance copy must not run on a runtime worker.
    let _ = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(&cleanup)).await;
    result?;
    Ok(out)
}

async fn export_inner(
    paths: &KbPaths,
    kb: &KbName,
    opts: &ExportOptions,
    staging: &Path,
    out: &Path,
    snapshot_started: SystemTime,
) -> Result<()> {
    let members = packed_members(paths, kb, opts.include_daemon);
    let staging_owned = staging.to_path_buf();
    let kb_owned = kb.clone();
    let stage_members = members.clone();
    // Names relative to `staging`, in tar argument order.
    let top_level: Vec<String> = tokio::task::spawn_blocking(move || {
        let staged_kb = staging_owned.join(kb_owned.as_str());
        std::fs::create_dir_all(&staged_kb)?;
        let mut top = vec![kb_owned.as_str().to_string()];
        for m in &stage_members {
            let dest = match m.scope {
                StateScope::PerKb => staged_kb.join(m.name),
                StateScope::Daemon => staging_owned.join(m.name),
            };
            if m.scope == StateScope::Daemon && m.path.exists() {
                top.push(m.name.to_string());
            }
            match m.class {
                StateClass::Persistent(Snapshot::VacuumInto) => vacuum_into(&m.path, &dest)?,
                StateClass::Persistent(Snapshot::CopyValidated) if m.path.exists() => {
                    copy_lance_dir(&m.path, &dest)?
                }
                StateClass::Persistent(_) if m.path.is_dir() => copy_dir(&m.path, &dest)?,
                StateClass::Persistent(_) if m.path.is_file() => {
                    std::fs::copy(&m.path, &dest)?;
                }
                _ => {}
            }
        }
        Ok::<Vec<String>, crate::Error>(top)
    })
    .await
    .map_err(|e| crate::Error::Storage(format!("backup staging task failed: {e}")))??;

    let staged_lance = staging.join(kb.as_str()).join("lance");
    if staged_lance.exists() {
        let store = crate::storage::lance::Storage::open(&staged_lance, None)
            .await
            .map_err(|e| {
                crate::Error::Storage(format!(
                    "lance snapshot is inconsistent ({e}); a commit may have landed \
                     mid-copy — retry, or stop the daemon for a guaranteed snapshot"
                ))
            })?;
        store.count_rows().await.map_err(|e| {
            crate::Error::Storage(format!("lance snapshot failed validation ({e})"))
        })?;
        // `open` + `count_rows` answer from manifest metadata alone, so they
        // cannot notice a manifest that names a data file the copy missed.
        verify_staged_lance_data_files(&staged_lance).await?;
    }

    let tar_staging = staging.to_path_buf();
    let tar_out = out.to_path_buf();
    tokio::task::spawn_blocking(move || {
        tar_tree(&tar_staging, &tar_out, &top_level, snapshot_started)
    })
    .await
    .map_err(|e| crate::Error::Storage(format!("backup tar task failed: {e}")))??;
    Ok(())
}

/// Keep the newest `keep` scheduled tarballs of `kb` in `exports` and
/// delete the rest, with their `.uploaded` markers. Returns what it
/// removed. `keep == 0` removes nothing (pruning disabled). Only names
/// [`is_scheduled_tarball_name`] accepts for exactly this kb are touched,
/// so `--out` files, other kbs and `notes-extra-…` siblings are safe.
/// Orphaned `<name>.<pid>.partial` files for this kb older than a day —
/// a crash mid-tar — are removed too.
pub fn prune_scheduled_exports(exports: &Path, kb: &str, keep: usize) -> Vec<PathBuf> {
    let mut removed = Vec::new();
    let Ok(entries) = std::fs::read_dir(exports) else {
        return removed;
    };
    let mut tarballs: Vec<(SystemTime, String, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Ok(mtime) = entry.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        if is_scheduled_tarball_name(name, kb) {
            tarballs.push((mtime, name.to_string(), entry.path()));
        } else if partial_target(name).is_some_and(|t| is_scheduled_tarball_name(t, kb))
            && mtime
                .elapsed()
                .is_ok_and(|age| age > std::time::Duration::from_secs(86_400))
            && std::fs::remove_file(entry.path()).is_ok()
        {
            removed.push(entry.path());
        }
    }
    if keep == 0 {
        return removed;
    }
    tarballs.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    for (_, _, path) in tarballs.into_iter().skip(keep) {
        if std::fs::remove_file(&path).is_ok() {
            let _ = std::fs::remove_file(uploaded_marker(&path));
            removed.push(path);
        }
    }
    removed
}

/// `<name>.<pid>.partial` → `<name>`.
fn partial_target(name: &str) -> Option<&str> {
    let rest = name.strip_suffix(".partial")?;
    let (target, pid) = rest.rsplit_once('.')?;
    (!pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit())).then_some(target)
}

/// The newest scheduled tarball of `kb` and its mtime.
pub fn newest_scheduled_tarball(exports: &Path, kb: &str) -> Option<(PathBuf, SystemTime)> {
    let mut newest: Option<(PathBuf, SystemTime)> = None;
    for entry in std::fs::read_dir(exports).ok()?.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !is_scheduled_tarball_name(name, kb) {
            continue;
        }
        let Ok(mtime) = entry.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        if newest.as_ref().is_none_or(|(_, prev)| mtime > *prev) {
            newest = Some((entry.path(), mtime));
        }
    }
    newest
}

fn newest_scheduled_tarball_mtime(exports: &Path, kb: &str) -> Option<SystemTime> {
    let entries = std::fs::read_dir(exports).ok()?;
    let mut newest = None;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !is_scheduled_tarball_name(name, kb) {
            continue;
        }
        let Ok(mtime) = entry.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        newest = Some(match newest {
            Some(prev) if prev >= mtime => prev,
            _ => mtime,
        });
    }
    newest
}

/// `<kb>-YYYYMMDD-HHMMSS.tar.gz`. The stamp shape keeps `notes` from
/// matching `notes-extra-YYYYMMDD-HHMMSS.tar.gz`, and ignores `--out`
/// names and `.staging-*` dirs.
fn is_scheduled_tarball_name(name: &str, kb: &str) -> bool {
    if kb.is_empty() {
        return false;
    }
    let Some(rest) = name.strip_prefix(kb) else {
        return false;
    };
    let Some(rest) = rest.strip_prefix('-') else {
        return false;
    };
    let Some(stamp) = rest.strip_suffix(".tar.gz") else {
        return false;
    };
    let b = stamp.as_bytes();
    b.len() == 15
        && b[8] == b'-'
        && b[..8].iter().all(u8::is_ascii_digit)
        && b[9..].iter().all(u8::is_ascii_digit)
}

fn index_write_mtime(index_db: &Path) -> Option<SystemTime> {
    let wal = sidecar(index_db, "-wal");
    newest_mtime([index_db, wal.as_path()])
}

fn sidecar(index_db: &Path, suffix: &str) -> PathBuf {
    let mut os = index_db.as_os_str().to_owned();
    os.push(suffix);
    PathBuf::from(os)
}

fn newest_mtime<'a>(paths: impl IntoIterator<Item = &'a Path>) -> Option<SystemTime> {
    let mut newest = None;
    for path in paths {
        let Ok(mtime) = std::fs::metadata(path).and_then(|m| m.modified()) else {
            continue;
        };
        newest = Some(match newest {
            Some(prev) if prev >= mtime => prev,
            _ => mtime,
        });
    }
    newest
}

fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    for entry in walkdir::WalkDir::new(src) {
        let entry =
            entry.map_err(|e| std::io::Error::other(format!("walk {}: {e}", src.display())))?;
        let rel = entry
            .path()
            .strip_prefix(src)
            .map_err(|e| std::io::Error::other(format!("strip {}: {e}", entry.path().display())))?;
        let target = dst.join(rel);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&target)?;
        } else {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Top-level entries of a lance dataset in COPY order: every metadata
/// entry (`_versions/`, `_transactions/`, `_deletions/`, `_indices/`, …)
/// first, `data/` last.
///
/// The dataset is copied while the daemon may be committing. Data files
/// are immutable and a manifest only names files that already exist, so
/// copying the manifests FIRST means every file they reference is there by
/// the time `data/` is walked. The reverse order (a plain `walkdir`, which
/// is `read_dir` order) could stage a manifest for version N+1 whose data
/// file was never copied — and `Storage::open` + `count_rows` answer from
/// manifest metadata alone, so validation could not notice.
pub fn lance_copy_order(src: &Path) -> Result<Vec<PathBuf>> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(src)?
        .collect::<std::io::Result<Vec<_>>>()?
        .into_iter()
        .map(|e| e.path())
        .collect();
    entries.sort_by_key(|p| {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        (name == "data", name.to_string())
    });
    Ok(entries)
}

/// Check that every data file named by the NEWEST staged manifest of every
/// lance table under `lance_root` exists in the staging copy.
///
/// Metadata-first copy order narrows the race with a committing daemon but
/// does not prove it; `Storage::open` + `count_rows` never touch data files.
/// This is the proof. A missing file fails the export (retry, or stop the
/// daemon) rather than shipping a tarball that cannot be read back.
///
/// Scope: only data files named by the manifest are checked; deletion files
/// and index files the manifest references are not.
pub async fn verify_staged_lance_data_files(lance_root: &Path) -> Result<()> {
    for entry in std::fs::read_dir(lance_root)? {
        let table_dir = entry?.path();
        if !table_dir.is_dir() || table_dir.extension().and_then(|e| e.to_str()) != Some("lance") {
            continue;
        }
        let uri = table_dir.to_string_lossy().into_owned();
        let ds = lance::Dataset::open(&uri).await.map_err(|e| {
            crate::Error::Storage(format!(
                "lance snapshot table {} failed to open ({e})",
                table_dir.display()
            ))
        })?;
        let data_dir = table_dir.join("data");
        for frag in ds.fragments().iter() {
            for f in &frag.files {
                if !data_dir.join(&f.path).is_file() {
                    return Err(crate::Error::Storage(format!(
                        "lance snapshot is inconsistent: manifest of {} names data file {} \
                         that is missing from the copy; a commit may have landed mid-copy — \
                         retry, or stop the daemon for a guaranteed snapshot",
                        table_dir
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("?"),
                        f.path
                    )));
                }
            }
        }
    }
    Ok(())
}

fn copy_lance_dir(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in lance_copy_order(src)? {
        let Some(name) = entry.file_name() else {
            continue;
        };
        let target = dst.join(name);
        if entry.is_dir() {
            copy_dir(&entry, &target)?;
        } else {
            std::fs::copy(&entry, &target)?;
        }
    }
    Ok(())
}

/// Pack `top_level` (names relative to `staging`) into `out` with `tar`
/// directly. Not a shell, and not the `kb` binary. Writes to a sibling
/// `.partial`, stamps it with `snapshot_started`, then renames; any
/// failure removes the partial and leaves `out` untouched.
fn tar_tree(
    staging: &Path,
    out: &Path,
    top_level: &[String],
    snapshot_started: SystemTime,
) -> Result<()> {
    let partial = sidecar(out, &format!(".{}.partial", std::process::id()));
    let mut cmd = Command::new("tar");
    cmd.arg("-czf")
        .arg(&partial)
        .arg("-C")
        .arg(staging)
        .arg("--");
    for name in top_level {
        cmd.arg(name);
    }
    let status = match cmd.status() {
        Ok(st) => st,
        Err(e) => {
            let _ = std::fs::remove_file(&partial);
            return Err(crate::Error::Storage(format!(
                "tar invocation failed (is `tar` installed?): {e}"
            )));
        }
    };
    if !status.success() {
        let _ = std::fs::remove_file(&partial);
        return Err(crate::Error::Storage(format!("tar exited {status}")));
    }
    let finish = std::fs::File::options()
        .write(true)
        .open(&partial)
        .and_then(|f| f.set_modified(snapshot_started))
        .and_then(|()| std::fs::rename(&partial, out));
    if let Err(e) = finish {
        let _ = std::fs::remove_file(&partial);
        return Err(e.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn daemon_scope_kb_is_the_first_in_name_order_not_input_order() {
        assert_eq!(
            daemon_scope_kb(["sessions", "docs", "memory"]),
            Some("docs")
        );
        assert_eq!(daemon_scope_kb(["docs", "memory"]), Some("docs"));
        assert_eq!(daemon_scope_kb(std::iter::empty::<&str>()), None);
    }

    #[test]
    fn vacuum_into_produces_a_consistent_openable_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("index.db");
        // A real, migrated DB with a row to copy.
        {
            let mut db = crate::storage::sqlite::Db::open(&src).unwrap();
            db.history_record_search("hello", 1_700_000_000, "operator")
                .unwrap();
        }
        let dest = tmp.path().join("snapshot.db");
        vacuum_into(&src, &dest).unwrap();
        assert!(dest.exists(), "snapshot not written");

        // The copy opens, passes integrity_check, and carries the row.
        let snap = Connection::open(&dest).unwrap();
        let integrity: String = snap
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap();
        assert_eq!(integrity, "ok", "snapshot failed integrity_check");
        let n: i64 = snap
            .query_row("SELECT count(*) FROM history", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "snapshot lost the history row");
    }

    #[test]
    fn run_remote_copy_is_none_when_unconfigured() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("kb.tar.gz");
        std::fs::write(&src, b"tarball bytes").unwrap();
        assert_eq!(run_remote_copy(&BackupSection::default(), &src), None);
    }

    #[test]
    fn run_remote_copy_succeeds_and_substitutes_src_dest() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("kb.tar.gz");
        std::fs::write(&src, b"tarball bytes").unwrap();
        let dest = tmp.path().join("off-host-copy.tar.gz");
        let cfg = BackupSection {
            remote_cmd: Some(vec!["cp".into(), "{src}".into(), "{dest}".into()]),
            remote_dest: Some(dest.to_string_lossy().into_owned()),
            ..Default::default()
        };
        let outcome = run_remote_copy(&cfg, &src);
        assert_eq!(outcome, Some(RemoteCopyOutcome::Ok));
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            b"tarball bytes",
            "remote {{dest}} did not receive the src bytes"
        );
    }

    #[test]
    fn run_remote_copy_surfaces_nonzero_exit_as_failed_without_erroring() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("kb.tar.gz");
        std::fs::write(&src, b"tarball bytes").unwrap();
        let script = tmp.path().join("fail-uploader.sh");
        std::fs::write(&script, "#!/bin/sh\necho boom-from-uploader >&2\nexit 1\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cfg = BackupSection {
            remote_cmd: Some(vec![
                script.to_string_lossy().into_owned(),
                "{src}".into(),
                "{dest}".into(),
            ]),
            remote_dest: Some("remote:bucket/path".into()),
            ..Default::default()
        };
        match run_remote_copy(&cfg, &src) {
            Some(RemoteCopyOutcome::Failed { message }) => {
                assert!(
                    message.contains("boom-from-uploader"),
                    "failure message should surface stderr: {message}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn run_remote_copy_does_not_hang_on_a_daemonised_child_holding_the_pipes() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("kb.tar.gz");
        std::fs::write(&src, b"tarball bytes").unwrap();
        let script = tmp.path().join("daemonising-uploader.sh");
        // Exits 0 at once, but leaves a background child that inherited (and
        // keeps open) the stdout/stderr pipes for far longer than the test.
        std::fs::write(&script, "#!/bin/sh\nsleep 25 &\nexit 0\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cfg = BackupSection {
            remote_cmd: Some(vec![script.to_string_lossy().into_owned(), "{src}".into()]),
            remote_dest: Some("remote:bucket/path".into()),
            ..Default::default()
        };
        let t0 = std::time::Instant::now();
        let outcome = run_remote_copy(&cfg, &src);
        let took = t0.elapsed();
        assert_eq!(outcome, Some(RemoteCopyOutcome::Ok));
        assert!(
            took < std::time::Duration::from_secs(10),
            "drain join must be bounded after a normal exit, took {took:?}"
        );
    }

    #[test]
    fn run_remote_copy_surfaces_spawn_error_as_failed() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("kb.tar.gz");
        std::fs::write(&src, b"tarball bytes").unwrap();
        let cfg = BackupSection {
            remote_cmd: Some(vec!["/no/such/uploader-binary-xyz".into(), "{src}".into()]),
            remote_dest: Some("remote:bucket/path".into()),
            ..Default::default()
        };
        match run_remote_copy(&cfg, &src) {
            Some(RemoteCopyOutcome::Failed { message }) => {
                assert!(
                    message.contains("failed to spawn"),
                    "expected a spawn-error message: {message}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn vacuum_into_refuses_to_overwrite_existing_dest() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("index.db");
        crate::storage::sqlite::Db::open(&src).unwrap();
        let dest = tmp.path().join("exists.db");
        std::fs::write(&dest, b"in the way").unwrap();
        assert!(
            vacuum_into(&src, &dest).is_err(),
            "VACUUM INTO must refuse an existing destination"
        );
    }

    fn set_mtime(path: &Path, when: SystemTime) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }

    fn epoch_plus(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs)
    }

    /// A laid-out `KbPaths` with one kb whose index, every packed tree,
    /// and (optionally) one scheduled tarball carry known mtimes.
    struct SkipFixture {
        _tmp: tempfile::TempDir,
        paths: KbPaths,
        kb: KbName,
    }

    impl SkipFixture {
        /// `tar_mtime` is the newest scheduled tarball's stamp; pass
        /// `None` for "no tarball yet". Every source starts `older` than
        /// that stamp, so a fixture built with one skips.
        fn new(tar_mtime: Option<u64>) -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let paths = KbPaths::rooted_at(tmp.path(), "daemon");
            let kb = KbName::new("notes").unwrap();
            std::fs::create_dir_all(paths.kb_state(&kb)).unwrap();
            std::fs::create_dir_all(&paths.exports).unwrap();
            std::fs::write(paths.kb_sqlite(&kb), b"db").unwrap();
            let older = epoch_plus(1_700_000_000);
            set_mtime(&paths.kb_sqlite(&kb), older);
            if let Some(secs) = tar_mtime {
                let tar = paths.exports.join("notes-20260101-000000.tar.gz");
                std::fs::write(&tar, b"tar").unwrap();
                set_mtime(&tar, epoch_plus(secs));
            }
            Self {
                _tmp: tmp,
                paths,
                kb,
            }
        }

        fn skip(&self) -> bool {
            should_skip_scheduled_backup(&self.paths, &self.kb, true)
        }

        /// Touch `path` to a stamp newer than the fixture's tarball, and
        /// touch its parent directory too — that is what the real atomic
        /// sidecar write does.
        fn touch_newer(&self, path: &Path) {
            // STRICTLY newer than the fixture's tarball stamp. The probe
            // compares `mtime > since`, so stamping the file at the SAME
            // second the tarball carries is indistinguishable from unchanged
            // and the test failed for a clock reason, not a predicate one.
            let newer = epoch_plus(1_700_003_601);
            set_mtime(path, newer);
            if let Some(parent) = path.parent() {
                set_mtime_dir(parent, newer);
            }
        }

        /// Write a file the PACKER would pack, at the path the PROBE walks.
        ///
        /// `KbPaths` resolves a kb's trees as `<state>/<kb>/{.review,lance}`
        /// (paths.rs:111-130) and the slates ledger as `<state>/slates`, so
        /// the segments are routed through the same helpers rather than
        /// hand-joined — a hand-join put `notes/.review/...` at the wrong
        /// depth, the probe never saw the write, and the test failed for a
        /// fixture reason instead of a predicate one.
        fn write_packed(&self, rel: &[&str], bytes: &[u8]) -> PathBuf {
            let path = match rel {
                [".review", rest @ ..] => rest
                    .iter()
                    .fold(self.paths.kb_review_dir(&self.kb), |a, s| a.join(s)),
                ["lance", rest @ ..] => rest
                    .iter()
                    .fold(self.paths.kb_lance(&self.kb), |a, s| a.join(s)),
                [".attachments", rest @ ..] => rest.iter().fold(
                    self.paths.kb_state(&self.kb).join(".attachments"),
                    |a, s| a.join(s),
                ),
                [".proposals", rest @ ..] => rest
                    .iter()
                    .fold(self.paths.kb_proposals_dir(&self.kb), |a, s| a.join(s)),
                ["slates", rest @ ..] => rest
                    .iter()
                    .fold(self.paths.state.join("slates"), |a, s| a.join(s)),
                other => panic!("write_packed: unmapped path {other:?}"),
            };
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, bytes).unwrap();
            self.touch_newer(&path);
            path
        }
    }

    fn set_mtime_dir(path: &Path, when: SystemTime) {
        // Directory mtimes are what a rename-replace or an unlink bumps.
        let f = std::fs::File::open(path).unwrap();
        f.set_modified(when).unwrap();
    }

    #[test]
    fn scheduled_backup_skips_only_when_index_is_not_newer_than_its_tarball() {
        let f = SkipFixture::new(None);
        assert!(!f.skip(), "index with no tarball must be written");

        // No index → nothing to snapshot (the one direction that skips
        // without any tarball existing).
        std::fs::remove_file(f.paths.kb_sqlite(&f.kb)).unwrap();
        assert!(f.skip(), "no index → nothing to snapshot");

        let f = SkipFixture::new(Some(1_700_003_600));
        // A sibling kb, a manual --out name, and a staging dir are not this kb's tarball.
        std::fs::write(
            f.paths.exports.join("notes-extra-20260102-000000.tar.gz"),
            b"other",
        )
        .unwrap();
        std::fs::write(f.paths.exports.join("notes-manual.tar.gz"), b"manual").unwrap();
        std::fs::create_dir(f.paths.exports.join(".staging-notes-1-20260101-000000")).unwrap();
        assert!(
            f.skip(),
            "unrelated exports must not count as notes' newest tarball"
        );

        // Newest by mtime, not by name, and an equal stamp is not a write.
        let by_name = f.paths.exports.join("notes-20260102-000000.tar.gz");
        std::fs::write(&by_name, b"older-bytes").unwrap();
        set_mtime(&by_name, epoch_plus(1_700_003_000));
        assert!(
            f.skip(),
            "the newest tarball is by mtime and the index is not newer"
        );

        set_mtime(&f.paths.kb_sqlite(&f.kb), epoch_plus(1_700_003_600));
        assert!(f.skip(), "equal mtime is not a write since the tarball");

        set_mtime(&f.paths.kb_sqlite(&f.kb), epoch_plus(1_700_003_601));
        assert!(
            !f.skip(),
            "an index write after the newest tarball must not be skipped"
        );
    }

    #[test]
    fn scheduled_backup_treats_wal_mtime_as_an_index_write() {
        let f = SkipFixture::new(Some(1_700_003_600));
        let wal = {
            let mut os = f.paths.kb_sqlite(&f.kb).into_os_string();
            os.push("-wal");
            PathBuf::from(os)
        };
        std::fs::write(&wal, b"wal").unwrap();
        set_mtime(&wal, epoch_plus(1_700_003_700));
        assert!(
            !f.skip(),
            "a WAL newer than the tarball is a write the main db mtime can miss"
        );
    }

    /// O4 — the three families `write_kb_export` packs that are NOT the
    /// sqlite index. None of them writes to sqlite: `slates/` appends
    /// JSONL, a comment/tag/anchor edit rewrites `.review/<id>.json`, and
    /// a lance commit lands in the dataset directory. A predicate that
    /// stat'd only `index.db` skipped every one of them, so the review
    /// ledger and the cross-agent slate were the artifacts most likely
    /// to be missing from the only backup that existed.
    #[test]
    fn scheduled_backup_watches_review_slates_and_lance_not_just_the_index() {
        for (label, rel) in [
            ("review sidecar", vec![".review", "c_abc.json"]),
            ("slate ledger", vec!["slates", "proj", "ledger.jsonl"]),
            ("lance commit", vec!["lance", "chunks", "0.lance"]),
            // Invariant #6 families that used to be packed by nothing and
            // watched by nothing.
            (
                "attachment blob",
                vec![".attachments", "art1", "a_0123456789ab"],
            ),
            ("queued proposal", vec![".proposals", "p_abc.json"]),
        ] {
            let f = SkipFixture::new(Some(1_700_003_600));
            assert!(f.skip(), "{label}: an untouched kb must still skip");
            let path = f.write_packed(&rel, b"{}");
            assert!(
                !f.skip(),
                "{label}: a write newer than the tarball must not be skipped"
            );
            // A rename-replace can leave the DIRECTORY as the only fresh
            // mtime: an importer restoring sidecars from a tarball, or
            // any tool that preserves stamps, hands the replacement an
            // older one. Watching files alone would call that unchanged.
            set_mtime(&path, epoch_plus(1_700_000_000));
            assert!(
                !f.skip(),
                "{label}: a packed tree whose only fresh mtime is its \
                 directory must still count as changed"
            );
        }
    }

    /// The probe must never report "unchanged" for something it could not
    /// finish looking at: an exhausted budget is `Unknown`, and an
    /// unpacked tree is `Absent` — neither may read as "unchanged".
    #[test]
    fn an_unfinished_change_probe_writes_instead_of_skipping() {
        let tmp = tempfile::tempdir().unwrap();
        let tree = tmp.path().join("tree");
        std::fs::create_dir_all(&tree).unwrap();
        std::fs::write(tree.join("a.json"), b"{}").unwrap();

        let mut budget = 0;
        assert!(
            matches!(probe_tree_mtime(&tree, &mut budget), TreeProbe::Unknown),
            "an exhausted budget must not look like an unchanged tree"
        );

        let mut budget = CHANGE_PROBE_ENTRY_BUDGET;
        assert!(
            matches!(probe_tree_mtime(&tree, &mut budget), TreeProbe::Newest(_)),
            "a walk that completes must report its newest mtime"
        );
        assert!(
            matches!(
                probe_tree_mtime(&tmp.path().join("absent"), &mut budget),
                TreeProbe::Absent
            ),
            "an unpacked tree is absent, not unknown"
        );
    }

    /// The composition that matters: a probe that cannot finish must make
    /// the PREDICATE write, not skip. Without this, "unknown" would
    /// silently mean "unchanged" again.
    #[test]
    fn an_unfinished_probe_on_a_packed_tree_forces_the_export() {
        let f = SkipFixture::new(Some(1_700_003_600));
        // Five aged sidecars: the walk cannot finish inside a budget of
        // two entries, and nothing in it is newer than the tarball.
        for i in 0..5 {
            let p = f.paths.kb_review_dir(&f.kb).join(format!("c_{i}.json"));
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, b"{}").unwrap();
            set_mtime(&p, epoch_plus(1_700_000_000));
        }
        set_mtime_dir(&f.paths.kb_review_dir(&f.kb), epoch_plus(1_700_000_000));
        assert!(
            skip_within_budget(&f.paths, &f.kb, true, 8),
            "a probe that finishes inside the budget skips an unchanged tree"
        );
        assert!(
            !skip_within_budget(&f.paths, &f.kb, true, 2),
            "a probe that runs out of budget must force the export"
        );
    }

    #[tokio::test]
    async fn write_kb_export_packs_restore_layout_without_the_kb_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = crate::paths::KbPaths::rooted_at(tmp.path(), "daemon");
        let kb = crate::types::KbName::new("notes").unwrap();
        let missing = write_kb_export(&paths, &kb).await.unwrap_err();
        assert!(
            missing.to_string().contains("no index"),
            "missing index must fail closed, got {missing}"
        );
        assert!(
            !paths.kb_sqlite(&kb).exists(),
            "a refused export must not create the live index"
        );

        std::fs::create_dir_all(paths.kb_state(&kb)).unwrap();
        {
            let mut db = crate::storage::sqlite::Db::open(&paths.kb_sqlite(&kb)).unwrap();
            db.history_record_search("hello", 1_700_000_000, "operator")
                .unwrap();
        }
        std::fs::create_dir_all(paths.kb_review_dir(&kb)).unwrap();
        std::fs::write(paths.kb_review_dir(&kb).join("c_abc.json"), b"{}").unwrap();
        let slate = paths.state.join("slates").join("proj");
        std::fs::create_dir_all(&slate).unwrap();
        std::fs::write(slate.join("meta.json"), b"{}").unwrap();

        let out = write_kb_export(&paths, &kb).await.unwrap();
        assert_eq!(out.parent(), Some(paths.exports.as_path()));
        let name = out.file_name().unwrap().to_str().unwrap();
        assert!(
            is_scheduled_tarball_name(name, "notes"),
            "writer must use the scheduled name the skipper recognizes, got {name}"
        );

        let extract = tmp.path().join("extract");
        std::fs::create_dir(&extract).unwrap();
        let status = Command::new("tar")
            .arg("-xzf")
            .arg(&out)
            .arg("-C")
            .arg(&extract)
            .status()
            .unwrap();
        assert!(status.success(), "tar extract failed");
        assert!(extract.join("notes/index.db").is_file());
        assert!(extract.join("notes/.review/c_abc.json").is_file());
        assert!(extract.join("slates/proj/meta.json").is_file());
        let snap = Connection::open(extract.join("notes/index.db")).unwrap();
        let n: i64 = snap
            .query_row("SELECT count(*) FROM history", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "tarball lost the vacuumed history row");

        for ent in std::fs::read_dir(&paths.exports).unwrap() {
            let n = ent.unwrap().file_name();
            let n = n.to_str().unwrap();
            assert!(!n.starts_with(".staging-"), "staging left behind: {n}");
        }

        // The name this writer just produced is what the skipper keys on.
        let ancient = epoch_plus(1_000);
        set_mtime(&paths.kb_sqlite(&kb), ancient);
        let wal = {
            let mut os = paths.kb_sqlite(&kb).into_os_string();
            os.push("-wal");
            PathBuf::from(os)
        };
        if wal.exists() {
            set_mtime(&wal, ancient);
        }
        // Age every OTHER family this writer packed too: the skipper
        // watches them (O4), so leaving them at "now" would — correctly —
        // read as "changed since the tarball we just wrote".
        for tree in [paths.kb_review_dir(&kb), paths.state.join("slates")] {
            for ent in walkdir::WalkDir::new(&tree)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                if ent.file_type().is_dir() {
                    set_mtime_dir(ent.path(), ancient);
                } else {
                    set_mtime(ent.path(), ancient);
                }
            }
        }
        assert!(
            should_skip_scheduled_backup(&paths, &kb, true),
            "a kb whose every packed source predates the tarball this writer \
             just produced must be skipped"
        );

        // …and the next pure comment edit (zero sqlite writes) must unskip it.
        std::fs::write(
            paths.kb_review_dir(&kb).join("c_abc.json"),
            b"{\"edited\":true}",
        )
        .unwrap();
        assert!(
            !should_skip_scheduled_backup(&paths, &kb, true),
            "a sidecar edited after the newest tarball must not be skipped"
        );
    }

    fn list_tar(tarball: &Path) -> Vec<String> {
        let out = Command::new("tar")
            .arg("-tzf")
            .arg(tarball)
            .output()
            .unwrap();
        assert!(out.status.success(), "tar -tzf failed");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| l.trim_end_matches('/').to_string())
            .collect()
    }

    /// A kb with one real file in EVERY member of the registry, written
    /// through the same path helpers the routes use.
    fn kb_with_every_member(tmp: &Path) -> (KbPaths, KbName) {
        let paths = KbPaths::rooted_at(tmp, "daemon");
        let kb = KbName::new("notes").unwrap();
        std::fs::create_dir_all(paths.kb_state(&kb)).unwrap();
        {
            let mut db = crate::storage::sqlite::Db::open(&paths.kb_sqlite(&kb)).unwrap();
            db.history_record_search("hello", 1_700_000_000, "operator")
                .unwrap();
        }
        let write = |p: PathBuf, bytes: &[u8]| {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, bytes).unwrap();
        };
        write(paths.kb_review_file(&kb, "c_abc"), b"{}");
        write(
            paths.kb_attachment_blob(&kb, "art1", "a_0123456789ab"),
            b"PNGBYTES",
        );
        write(paths.kb_attachment_manifest(&kb, "art1"), b"{}");
        write(paths.kb_proposal_file(&kb, "p_abc"), b"{\"p\":1}");
        write(paths.state.join("slates/proj/ledger.jsonl"), b"{}\n");
        write(paths.saved_queries_file(), b"[]");
        write(paths.memory_policy_file(), b"{}");
        write(paths.tombstone_era_file(), b"{\"era\":1}");
        // Not persistent: must never be packed.
        write(paths.embed_cache_file(), b"cache");
        write(paths.daemon_pid_file(), b"1234");
        (paths, kb)
    }

    /// v0.44 B1 / A3-2 / I3 — invariant #6 says the sidecar family is
    /// "registered in backup". Before the registry, `.attachments/` and
    /// `.proposals/` were in neither writer, and `kb restore --force`
    /// (remove_dir_all, then extract) destroyed them. This writes one
    /// file per registry member and asserts the tarball carries exactly
    /// the persistent ones.
    #[tokio::test]
    async fn every_persistent_state_member_is_in_the_tarball_and_no_other() {
        let tmp = tempfile::tempdir().unwrap();
        let (paths, kb) = kb_with_every_member(tmp.path());
        let out = write_kb_export(&paths, &kb).await.unwrap();
        let listing = list_tar(&out);
        for want in [
            "notes/index.db",
            "notes/.review/c_abc.json",
            "notes/.attachments/art1/a_0123456789ab",
            "notes/.attachments/art1/_manifest.json",
            "notes/.proposals/p_abc.json",
            "slates/proj/ledger.jsonl",
            "saved-queries.json",
            "memory-policy.json",
            "tombstone-era.json",
        ] {
            assert!(
                listing.iter().any(|l| l == want),
                "{want} missing from the tarball: {listing:?}"
            );
        }
        for never in ["query-embed-cache.json", "kb-daemon.pid"] {
            assert!(
                !listing.iter().any(|l| l.contains(never)),
                "{never} is regenerable/runtime and must not be packed: {listing:?}"
            );
        }

        // Round trip: the attachment bytes survive extraction.
        let extract = tmp.path().join("extract");
        std::fs::create_dir(&extract).unwrap();
        assert!(Command::new("tar")
            .arg("-xzf")
            .arg(&out)
            .arg("-C")
            .arg(&extract)
            .status()
            .unwrap()
            .success());
        assert_eq!(
            std::fs::read(extract.join("notes/.attachments/art1/a_0123456789ab")).unwrap(),
            b"PNGBYTES"
        );
    }

    /// The guard A13-6 found missing: every entry the daemon puts under a
    /// kb's state dir (and under `<state>/`) must be classified by
    /// `state_members`. A new sidecar written without registering it
    /// fails here instead of being lost on the first restore.
    #[test]
    fn every_entry_under_state_is_classified_by_the_registry() {
        let tmp = tempfile::tempdir().unwrap();
        let (paths, kb) = kb_with_every_member(tmp.path());
        let members = paths.state_members(&kb);
        let per_kb: Vec<&str> = members
            .iter()
            .filter(|m| m.scope == StateScope::PerKb)
            .map(|m| m.name)
            .collect();
        for ent in std::fs::read_dir(paths.kb_state(&kb)).unwrap() {
            let name = ent.unwrap().file_name().into_string().unwrap();
            assert!(
                per_kb.contains(&name.as_str()) || name == "index.db-wal" || name == "index.db-shm",
                "{name} under <state>/<kb>/ is not in KbPaths::state_members"
            );
        }
        let daemon: Vec<&str> = members
            .iter()
            .filter(|m| m.scope == StateScope::Daemon)
            .map(|m| m.name)
            .collect();
        for ent in std::fs::read_dir(&paths.state).unwrap() {
            let name = ent.unwrap().file_name().into_string().unwrap();
            assert!(
                daemon.contains(&name.as_str())
                    || name == kb.as_str()
                    || ["exports", "quarantine", "runs"].contains(&name.as_str()),
                "{name} under <state>/ is not in KbPaths::state_members"
            );
        }
        // Every registry member is documented persistent / regenerable / runtime;
        // the persistent ones are exactly what the packer selects.
        let packed: Vec<&str> = packed_members(&paths, &kb, true)
            .iter()
            .map(|m| m.name)
            .collect();
        for m in &members {
            assert_eq!(
                packed.contains(&m.name),
                m.is_persistent(),
                "{} packed/persistent mismatch",
                m.name
            );
        }
    }

    /// A3-3 — slates are daemon-wide. A slate append must not make every
    /// kb "changed"; only the kb that carries them (include_daemon).
    #[test]
    fn a_slate_append_does_not_unskip_a_kb_that_does_not_carry_slates() {
        let f = SkipFixture::new(Some(1_700_003_600));
        f.write_packed(&["slates", "proj", "ledger.jsonl"], b"{}");
        assert!(
            !should_skip_scheduled_backup(&f.paths, &f.kb, true),
            "the designated kb must export a newer slate"
        );
        assert!(
            should_skip_scheduled_backup(&f.paths, &f.kb, false),
            "a kb that does not carry slates must not export for a slate append"
        );
    }

    /// A3-13 — the export's staging (the multi-GB copy walk and the staging
    /// cleanup) goes through the blocking pool. The runtime's only blocking
    /// thread is held by the test: an export doing that work inline on the
    /// async worker would complete anyway; one using `spawn_blocking` cannot.
    #[test]
    fn export_staging_runs_on_the_blocking_pool() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let tmp = tempfile::tempdir().unwrap();
            let (paths, kb) = kb_with_every_member(tmp.path());
            let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
            let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
            let hold = tokio::task::spawn_blocking(move || {
                let _ = started_tx.send(());
                let _ = release_rx.recv();
            });
            started_rx.await.unwrap();
            let opts = ExportOptions {
                include_daemon: false,
                out: None,
            };
            let fut = write_kb_export_with(&paths, &kb, &opts);
            tokio::pin!(fut);
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(300), &mut fut)
                    .await
                    .is_err(),
                "the export finished while the blocking pool was saturated: it ran on the async worker"
            );
            // The tar hop alone would keep the future pending, so pin the
            // staging step itself: its closure creates the staging dir, and
            // while the pool is held that closure cannot have started. An
            // inline staging walk would already have created (and filled) it.
            let staged_now: Vec<_> = std::fs::read_dir(&paths.exports)
                .map(|rd| {
                    rd.filter_map(|e| e.ok())
                        .filter(|e| e.file_name().to_string_lossy().starts_with(".staging-"))
                        .collect()
                })
                .unwrap_or_default();
            assert!(
                staged_now.is_empty(),
                "staging began while the blocking pool was saturated: the copy walk ran on the async worker"
            );
            release_tx.send(()).unwrap();
            let out = tokio::time::timeout(std::time::Duration::from_secs(30), &mut fut)
                .await
                .expect("completes once the blocking thread is free")
                .unwrap();
            assert!(out.is_file());
            hold.await.unwrap();
            let leftovers: Vec<_> = std::fs::read_dir(&paths.exports)
                .unwrap()
                .filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().starts_with(".staging-"))
                .collect();
            assert!(leftovers.is_empty(), "staging left behind");
        });
    }

    #[tokio::test]
    async fn daemon_members_are_packed_only_when_asked() {
        let tmp = tempfile::tempdir().unwrap();
        let (paths, kb) = kb_with_every_member(tmp.path());
        let out = write_kb_export_with(
            &paths,
            &kb,
            &ExportOptions {
                include_daemon: false,
                out: None,
            },
        )
        .await
        .unwrap();
        let listing = list_tar(&out);
        assert!(listing.iter().any(|l| l == "notes/.proposals/p_abc.json"));
        assert!(
            !listing
                .iter()
                .any(|l| l.starts_with("slates") || l.ends_with(".json") && !l.contains('/')),
            "daemon-scope members leaked into a per-kb tarball: {listing:?}"
        );
    }

    /// A4-1 / A3-9 / A4.f1 — a tar that fails leaves NOTHING under a name
    /// the skip predicate or doctor could read as a backup, and does not
    /// clobber an existing file at the destination.
    #[test]
    fn a_failed_tar_leaves_no_tarball_and_no_partial() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("notes-20260101-000000.tar.gz");
        std::fs::write(&out, b"previous good backup").unwrap();
        let err = tar_tree(
            &tmp.path().join("no-such-staging"),
            &out,
            &["notes".to_string()],
            SystemTime::now(),
        );
        assert!(err.is_err(), "tar of a missing tree must fail");
        assert_eq!(std::fs::read(&out).unwrap(), b"previous good backup");
        let leftovers: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| n.ends_with(".partial"))
            .collect();
        assert!(leftovers.is_empty(), "partial left behind: {leftovers:?}");

        let fresh = tmp.path().join("notes-20260102-000000.tar.gz");
        let _ = tar_tree(
            &tmp.path().join("no-such-staging"),
            &fresh,
            &["notes".to_string()],
            SystemTime::now(),
        );
        assert!(!fresh.exists(), "a failed tar must not create the tarball");
    }

    /// A3-9 — the tarball is stamped with the snapshot START, so a source
    /// edited while it was being built is newer than it.
    #[test]
    fn the_tarball_mtime_is_the_snapshot_start_not_the_finish() {
        let tmp = tempfile::tempdir().unwrap();
        let staging = tmp.path().join("staging");
        std::fs::create_dir_all(staging.join("notes")).unwrap();
        std::fs::write(staging.join("notes/index.db"), b"db").unwrap();
        let out = tmp.path().join("notes-20260101-000000.tar.gz");
        let started = epoch_plus(1_700_000_000);
        tar_tree(&staging, &out, &["notes".to_string()], started).unwrap();
        let mtime = std::fs::metadata(&out).unwrap().modified().unwrap();
        assert_eq!(mtime, started);
        assert!(
            std::fs::read_dir(tmp.path()).unwrap().all(|e| !e
                .unwrap()
                .file_name()
                .to_str()
                .unwrap()
                .ends_with(".partial")),
            "the partial must be renamed away"
        );
    }

    /// A3-8 — metadata (`_versions/` …) is copied before `data/`.
    #[test]
    fn lance_metadata_is_copied_before_data() {
        let tmp = tempfile::tempdir().unwrap();
        for d in [
            "data",
            "_versions",
            "_transactions",
            "_indices",
            "_deletions",
        ] {
            std::fs::create_dir_all(tmp.path().join(d)).unwrap();
        }
        let order = lance_copy_order(tmp.path()).unwrap();
        let names: Vec<String> = order
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap().to_string())
            .collect();
        assert_eq!(names.last().map(String::as_str), Some("data"), "{names:?}");
        let versions = names.iter().position(|n| n == "_versions").unwrap();
        let data = names.iter().position(|n| n == "data").unwrap();
        assert!(versions < data, "{names:?}");
    }

    /// A3-8 — a staged manifest naming a data file the copy missed fails the
    /// export check (open + count_rows alone would pass it).
    #[tokio::test]
    async fn staged_lance_with_a_missing_data_file_is_rejected() {
        use arrow::record_batch::RecordBatchIterator;
        use arrow_array::{Int32Array, RecordBatch};
        use arrow_schema::{DataType, Field, Schema};
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("lance");
        std::fs::create_dir_all(&root).unwrap();
        let table = root.join("t.lance");
        let schema =
            std::sync::Arc::new(Schema::new(vec![Field::new("a", DataType::Int32, false)]));
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![std::sync::Arc::new(Int32Array::from(vec![1, 2, 3]))],
        )
        .unwrap();
        let reader = RecordBatchIterator::new(vec![Ok(batch)], schema);
        lance::Dataset::write(reader, table.to_str().unwrap(), None)
            .await
            .unwrap();
        verify_staged_lance_data_files(&root)
            .await
            .expect("an intact copy verifies");
        let data = table.join("data");
        let mut removed = 0;
        for e in std::fs::read_dir(&data).unwrap() {
            std::fs::remove_file(e.unwrap().path()).unwrap();
            removed += 1;
        }
        assert!(removed > 0, "the fixture must have written a data file");
        let err = verify_staged_lance_data_files(&root).await.unwrap_err();
        assert!(err.to_string().contains("missing from the copy"), "{err}");
    }

    /// A3-8 (X5) — the EXPORT wiring, not just the verifier: a lance data file
    /// that the staged copy lacks (here: absent from the source, which is what
    /// a copy racing a compaction leaves behind in staging) must fail
    /// `write_kb_export` and leave no tarball. `Storage::open` + `count_rows`
    /// pass on such a copy, so only the `verify_staged_lance_data_files` call
    /// inside `export_inner` can refuse it; delete that call and this fails.
    #[tokio::test]
    async fn export_fails_when_a_lance_data_file_is_missing_from_the_copy() {
        use arrow::record_batch::RecordBatchIterator;
        use arrow_array::{Int32Array, RecordBatch};
        use arrow_schema::{DataType, Field, Schema};
        let tmp = tempfile::tempdir().unwrap();
        let paths = KbPaths::rooted_at(tmp.path(), "daemon");
        let kb = KbName::new("notes").unwrap();
        std::fs::create_dir_all(paths.kb_state(&kb)).unwrap();
        drop(crate::storage::sqlite::Db::open(&paths.kb_sqlite(&kb)).unwrap());

        let table = paths.kb_lance(&kb).join("t.lance");
        std::fs::create_dir_all(paths.kb_lance(&kb)).unwrap();
        let schema =
            std::sync::Arc::new(Schema::new(vec![Field::new("a", DataType::Int32, false)]));
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![std::sync::Arc::new(Int32Array::from(vec![1, 2, 3]))],
        )
        .unwrap();
        lance::Dataset::write(
            RecordBatchIterator::new(vec![Ok(batch)], schema),
            table.to_str().unwrap(),
            None,
        )
        .await
        .unwrap();

        // Control: the intact dataset exports.
        write_kb_export(&paths, &kb)
            .await
            .expect("an intact lance dataset exports");

        let mut removed = 0;
        for e in std::fs::read_dir(table.join("data")).unwrap() {
            std::fs::remove_file(e.unwrap().path()).unwrap();
            removed += 1;
        }
        assert!(removed > 0, "the fixture must have written a data file");
        for e in std::fs::read_dir(&paths.exports).unwrap() {
            let _ = std::fs::remove_file(e.unwrap().path());
        }

        let err = write_kb_export(&paths, &kb).await.unwrap_err();
        assert!(err.to_string().contains("missing from the copy"), "{err}");
        let leftovers: Vec<_> = std::fs::read_dir(&paths.exports)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            leftovers.is_empty(),
            "a refused export leaves no tarball, partial or staging dir: {leftovers:?}"
        );
    }

    /// A3-3 — retention keeps the newest K per kb and touches nothing else.
    #[test]
    fn prune_keeps_the_newest_k_per_kb_and_only_that_kbs() {
        let tmp = tempfile::tempdir().unwrap();
        let ex = tmp.path();
        for (i, day) in ["01", "02", "03", "04"].iter().enumerate() {
            let f = ex.join(format!("notes-202601{day}-000000.tar.gz"));
            std::fs::write(&f, b"t").unwrap();
            set_mtime(&f, epoch_plus(1_700_000_000 + i as u64 * 100));
            mark_uploaded(&f).unwrap();
        }
        for other in [
            "docs-20260101-000000.tar.gz",
            "notes-extra-20260101-000000.tar.gz",
            "notes-manual.tar.gz",
        ] {
            std::fs::write(ex.join(other), b"keep").unwrap();
        }
        let removed = prune_scheduled_exports(ex, "notes", 2);
        assert_eq!(removed.len(), 2, "{removed:?}");
        assert!(!ex.join("notes-20260101-000000.tar.gz").exists());
        assert!(!uploaded_marker(&ex.join("notes-20260101-000000.tar.gz")).exists());
        assert!(!ex.join("notes-20260102-000000.tar.gz").exists());
        assert!(ex.join("notes-20260103-000000.tar.gz").exists());
        assert!(ex.join("notes-20260104-000000.tar.gz").exists());
        assert!(uploaded_marker(&ex.join("notes-20260104-000000.tar.gz")).exists());
        for other in [
            "docs-20260101-000000.tar.gz",
            "notes-extra-20260101-000000.tar.gz",
            "notes-manual.tar.gz",
        ] {
            assert!(ex.join(other).exists(), "{other} must not be pruned");
        }
        assert!(prune_scheduled_exports(ex, "notes", 0).is_empty());
    }

    /// A3-4 — a stalled uploader is killed at the deadline (with its
    /// children) and reported as a failed copy instead of hanging forever.
    #[test]
    fn a_hung_uploader_is_killed_at_the_deadline() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("kb.tar.gz");
        std::fs::write(&src, b"x").unwrap();
        let cfg = BackupSection {
            remote_cmd: Some(vec![
                "sh".into(),
                "-c".into(),
                "sleep 60; echo {src} {dest}".into(),
            ]),
            remote_dest: Some("remote:x".into()),
            remote_timeout_secs: Some(1),
            ..Default::default()
        };
        let started = std::time::Instant::now();
        match run_remote_copy(&cfg, &src) {
            Some(RemoteCopyOutcome::Failed { message }) => {
                assert!(message.contains("timed out"), "{message}");
            }
            other => panic!("expected a timeout failure, got {other:?}"),
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(30),
            "the deadline did not bound the uploader"
        );
    }

    #[test]
    fn the_uploaded_marker_is_per_tarball() {
        let tmp = tempfile::tempdir().unwrap();
        let t = tmp.path().join("notes-20260101-000000.tar.gz");
        std::fs::write(&t, b"t").unwrap();
        assert!(!is_uploaded(&t));
        mark_uploaded(&t).unwrap();
        assert!(is_uploaded(&t));
        assert!(!is_uploaded(
            &tmp.path().join("notes-20260102-000000.tar.gz")
        ));
        // The marker does not itself look like a tarball to the skipper.
        let name = uploaded_marker(&t);
        assert!(!is_scheduled_tarball_name(
            name.file_name().unwrap().to_str().unwrap(),
            "notes"
        ));
    }
}
