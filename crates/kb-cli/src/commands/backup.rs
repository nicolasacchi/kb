//! `kb backup <kb> [--out PATH]` — write a CONSISTENT tarball snapshot of
//! a kb's persistent state (`index.db` + `lance/` + `.review/`) plus the
//! daemon-wide `slates/` store, under `<state>/exports/`.
//!
//! **sqlite is captured atomically** via `VACUUM INTO`
//! (`kb_core::storage::backup::vacuum_into`): a transactionally-consistent
//! copy even while the daemon writes — no torn `index.db`, and no
//! `-wal`/`-shm` sidecars to reconcile. (Pre-B1 this tar'd the live files
//! and could capture a half-applied transaction.)
//!
//! **lance is copied then validated** by re-opening the staged dataset.
//! lance fragments are immutable and manifests commit atomically, so a
//! clean open means a consistent version was captured. If the open fails
//! (a copy caught mid-commit — only possible under an actively-indexing
//! daemon), the backup errors and advises re-running with the daemon
//! paused/stopped. For a guaranteed-consistent lance snapshot under load,
//! stop the daemon first; the sqlite half is consistent regardless.
//!
//! **Scope: persistent state only.** The `RequestMetrics` instrumentation
//! lives in process RAM and is NOT covered (it resets on restart by
//! design). Restore a tarball with `kb restore <tarball> --kb <name>`.
//!
//! **GC-B4 — optional off-host copy.** When `[backup]` in `kb.toml` sets
//! both `remote_cmd` and `remote_dest`, a successful local backup is
//! followed by a best-effort remote copy
//! (`kb_core::storage::backup::run_remote_copy`): a failed or unreachable
//! remote target is loudly reported (stderr warning + an annotated stdout
//! summary line) but never fails the backup itself — the local tarball is
//! already a complete backup on its own.
//!
//! **`--all`.** [`run_all`] lists every corpus via `GET /api/kbs` and
//! snapshots each name the same way [`run`] does (default
//! `<state>/exports/<kb>-<timestamp>.tar.gz`). Every listed kb is
//! attempted. A failure is printed and kept; the command returns an error
//! naming every failed kb, so the process exits non-zero. It does not stop
//! at the first failure, and it does not report success when any kb failed.
//!
//! `--daemon` must be absent or loopback (`127.0.0.1`, `localhost`, `::1`,
//! or a URL with no host). A remote daemon is refused before the list and
//! before any tar — one stderr line, then an error — because the name list
//! would be remote while each snapshot is this machine's `KbPaths`.
//!
//! When `[backup]` off-host copy is configured, `--all` appends each
//! tarball's basename to `remote_dest`. A shared `{dest}` (`rclone copyto`)
//! would otherwise keep only the last corpus.

use super::{load_config_or_default, resolve_config_path};
use crate::http::client_with_timeout_and_bearer;
use anyhow::{anyhow, Context, Result};
use kb_core::paths::KbPaths;
use kb_core::storage::lance::Storage;
use kb_core::types::KbName;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

pub async fn run(config_path: Option<&PathBuf>, kb: &str, out: Option<&Path>) -> Result<()> {
    snapshot(config_path, kb, out, OffHostDest::Configured).await
}

async fn snapshot(
    config_path: Option<&PathBuf>,
    kb: &str,
    out: Option<&Path>,
    off_host: OffHostDest,
) -> Result<()> {
    let kb_name = KbName::new(kb).map_err(|e| anyhow!("invalid kb {kb:?}: {e}"))?;
    let cfg_path = resolve_config_path(config_path)?;
    let cfg = load_config_or_default(&cfg_path)?;
    let paths = KbPaths::new(cfg.daemon.name.as_deref().unwrap_or("default"))?;
    paths.ensure_dirs()?;

    let kb_state = paths.kb_state(&kb_name);
    if !kb_state.exists() {
        return Err(anyhow!(
            "kb {kb_name} has no state at {}; index something first",
            kb_state.display()
        ));
    }

    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let out_path = out
        .map(PathBuf::from)
        .unwrap_or_else(|| paths.exports.join(format!("{kb_name}-{stamp}.tar.gz")));
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Stage a consistent snapshot under exports/, tar it, then clean up
    // the staging dir unconditionally (success or error).
    let staging = paths
        .exports
        .join(format!(".staging-{kb_name}-{}-{stamp}", std::process::id()));
    let result = stage_and_tar(&paths, &kb_name, &staging, &out_path).await;
    let _ = std::fs::remove_dir_all(&staging);
    result?;

    println!("backup: {} → {}", kb_state.display(), out_path.display());

    // GC-B4 — best-effort off-host copy. Never fails the backup itself:
    // the local tarball above is already a complete, successful backup.
    // A configured-but-failed remote copy is surfaced loudly (stderr +
    // a plain stdout line so `kb backup`'s own exit message is
    // unmissable either way) but the command still exits 0.
    //
    // `--all` uses [`OffHostDest::PerCorpus`]: `copyto {src} {dest}` would
    // otherwise write every corpus onto the same object and keep only the
    // last. Append the tarball basename so each corpus is its own object.
    // A single-kb backup leaves `remote_dest` unchanged.
    let mut backup_cfg = cfg.backup.clone();
    if matches!(off_host, OffHostDest::PerCorpus) {
        if let Some(dest) = backup_cfg.remote_dest.clone() {
            backup_cfg.remote_dest = Some(remote_dest_for_corpus(&dest, &out_path));
        }
    }
    let shown_dest = backup_cfg.remote_dest.as_deref().unwrap_or("?");
    match kb_core::storage::backup::run_remote_copy(&backup_cfg, &out_path) {
        None => {}
        Some(kb_core::storage::backup::RemoteCopyOutcome::Ok) => {
            println!("backup: off-host copy → {shown_dest} ok");
        }
        Some(kb_core::storage::backup::RemoteCopyOutcome::Failed { message }) => {
            eprintln!(
                "backup: WARNING off-host copy to {shown_dest} FAILED: {message} \
                 (the local tarball at {} is intact; the backup itself succeeded)",
                out_path.display()
            );
            println!(
                "backup: {} → {} (off-host copy FAILED — see warning above)",
                kb_state.display(),
                out_path.display()
            );
        }
    }

    Ok(())
}

/// `kb backup --all` — one tarball per corpus listed by `GET /api/kbs`.
///
/// Snapshots each name the same way [`run`] does, with `out = None`, so
/// each kb lands at the default export path. `--out` is not a parameter:
/// one path cannot hold every tarball. When off-host copy is configured,
/// each tarball's basename is appended to `remote_dest` so `copyto` does
/// not keep only the last corpus.
///
/// `--daemon` must be absent or loopback (`127.0.0.1`, `localhost`, `::1`,
/// or a URL with no host — the default local daemon). A remote daemon is
/// refused before `GET /api/kbs` and before any tar: one stderr line, then
/// an error. Listing a remote daemon and tarring local `KbPaths` would
/// snapshot this machine under the remote's names.
///
/// Every listed name is attempted, including after a failure. Each failure
/// is printed to stderr (`backup: kb <name> FAILED: …`) and retained. If any
/// attempt failed, this returns an error naming every failed kb (the process
/// exits non-zero). A later success does not cancel an earlier failure.
///
/// An empty list is an error — exiting 0 after writing nothing would hide a
/// daemon that reported no corpora. An entry with no usable `name` fails the
/// listing before any tarball is written; it is not skipped.
pub async fn run_all(
    config_path: Option<&PathBuf>,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    if !backup_daemon_is_local(daemon) {
        let shown = daemon.map(str::trim).unwrap_or("");
        let line = remote_daemon_refusal(shown);
        eprintln!("{line}");
        return Err(anyhow!("{line}"));
    }
    let names = list_kb_names(daemon, bearer).await?;
    if names.is_empty() {
        return Err(anyhow!(
            "backup --all: GET /api/kbs returned no kbs; nothing was backed up"
        ));
    }
    let mut outcomes = Vec::with_capacity(names.len());
    for kb in &names {
        // Do not `?` here. A failed kb must be recorded, and every later kb
        // must still be backed up.
        let error = match snapshot(config_path, kb, None, OffHostDest::PerCorpus).await {
            Ok(()) => None,
            Err(e) => {
                let msg = format!("{e:#}");
                eprintln!("backup: kb {kb} FAILED: {msg}");
                Some(msg)
            }
        };
        outcomes.push(KbBackupOutcome {
            kb: kb.as_str(),
            error,
        });
    }
    aggregate_backup_failures(&outcomes)
}

const DEFAULT_DAEMON: &str = "http://127.0.0.1:4000";

/// How `{dest}` is passed to the off-host copy.
enum OffHostDest {
    /// Single corpus: the operator's `remote_dest` unchanged.
    Configured,
    /// `--all`: append the tarball basename so `copyto` does not clobber.
    PerCorpus,
}

/// Whether `backup --all` may list corpora from this daemon and tar local state.
///
/// `None`, empty, or a URL with no host is the default local daemon. A host
/// is allowed only when it is loopback (`127.0.0.1`, `localhost`, `::1`).
/// A remote name list must not drive local `KbPaths` snapshots.
fn backup_daemon_is_local(daemon: Option<&str>) -> bool {
    let Some(raw) = daemon.map(str::trim).filter(|s| !s.is_empty()) else {
        return true;
    };
    match parse_daemon_url(raw) {
        Some(url) => match url.host_str() {
            None | Some("") => true,
            Some(host) => host_is_loopback(host),
        },
        None => false,
    }
}

/// Parse a daemon URL. Scheme-less `host:port` is given `http://` so
/// `example:4000` is a host, not a scheme with an empty host (which would
/// look like "no host" and be treated as local).
fn parse_daemon_url(raw: &str) -> Option<reqwest::Url> {
    let trimmed = raw.trim();
    if trimmed.contains("://") {
        return reqwest::Url::parse(trimmed).ok();
    }
    let candidate = if trimmed.starts_with('[') {
        format!("http://{trimmed}")
    } else if trimmed.contains("::") {
        format!("http://[{trimmed}]")
    } else {
        format!("http://{trimmed}")
    };
    reqwest::Url::parse(&candidate).ok()
}

fn host_is_loopback(host: &str) -> bool {
    let host = host.trim_matches(|c| c == '[' || c == ']');
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(v4)) => v4 == std::net::Ipv4Addr::LOCALHOST,
        Ok(std::net::IpAddr::V6(v6)) => v6.is_loopback(),
        Err(_) => false,
    }
}

fn remote_daemon_refusal(daemon: &str) -> String {
    format!(
        "backup --all: refusing --daemon {daemon} — not a loopback daemon; \
         --all snapshots this machine and will not tar local state for a remote name list"
    )
}

/// `{dest}/{tarball-basename}` so `--all` does not `copyto` every corpus
/// onto one object. A trailing slash on `dest` is not doubled.
fn remote_dest_for_corpus(dest: &str, tarball: &Path) -> String {
    let base = tarball
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty())
        .unwrap_or("backup.tar.gz");
    let dest = dest.trim_end_matches('/');
    format!("{dest}/{base}")
}

async fn list_kb_names(daemon: Option<&str>, bearer: Option<&str>) -> Result<Vec<String>> {
    let base = daemon.unwrap_or(DEFAULT_DAEMON).trim_end_matches('/');
    let url = format!("{base}/api/kbs");
    let client = client_with_timeout_and_bearer(5, bearer)?;
    let body: Value = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("GET {url}"))?
        .json()
        .await
        .with_context(|| format!("parse JSON from {url}"))?;
    kb_names_from_api(&body).with_context(|| format!("GET {url}"))
}

/// Names from a `GET /api/kbs` body, in response order.
///
/// A non-array body, a missing or non-string `name`, or an empty `name` is
/// an error. Entries are never dropped — skipping one would omit a corpus
/// from the backup set.
fn kb_names_from_api(body: &Value) -> Result<Vec<String>> {
    let arr = body
        .as_array()
        .ok_or_else(|| anyhow!("GET /api/kbs: expected an array"))?;
    let mut names = Vec::with_capacity(arr.len());
    for (i, kb) in arr.iter().enumerate() {
        let name = kb["name"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("GET /api/kbs: entry {i} has no name"))?;
        names.push(name.to_string());
    }
    Ok(names)
}

/// One kb in a `--all` sweep. `error: Some` is a failure, including an empty
/// string — never treated as success.
struct KbBackupOutcome<'a> {
    kb: &'a str,
    error: Option<String>,
}

/// `Err` if any outcome failed. The error names every failed kb, in list
/// order, with each failure text. Successes are omitted. An empty slice, or
/// a slice of only successes, is `Ok`.
fn aggregate_backup_failures(outcomes: &[KbBackupOutcome<'_>]) -> Result<()> {
    let failed: Vec<&KbBackupOutcome<'_>> = outcomes.iter().filter(|o| o.error.is_some()).collect();
    if failed.is_empty() {
        return Ok(());
    }
    let listed = failed
        .iter()
        .map(|o| {
            let why = o
                .error
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or("unknown error");
            format!("{} ({why})", o.kb)
        })
        .collect::<Vec<_>>()
        .join("; ");
    Err(anyhow!(
        "backup --all: {} kb(s) failed: {listed}",
        failed.len()
    ))
}

/// Build the consistent snapshot tree at `staging/<kb>/` and tar it.
async fn stage_and_tar(
    paths: &KbPaths,
    kb_name: &KbName,
    staging: &Path,
    out_path: &Path,
) -> Result<()> {
    let staged_kb = staging.join(kb_name.as_str());
    std::fs::create_dir_all(&staged_kb)?;

    // 1. sqlite — transactionally-consistent copy (no torn DB).
    kb_core::storage::backup::vacuum_into(&paths.kb_sqlite(kb_name), &staged_kb.join("index.db"))
        .map_err(|e| anyhow!("sqlite snapshot failed: {e}"))?;

    // 2. lance — copy, then validate by re-opening the staged dataset.
    let src_lance = paths.kb_lance(kb_name);
    if src_lance.exists() {
        let staged_lance = staged_kb.join("lance");
        copy_dir(&src_lance, &staged_lance)?;
        let store = Storage::open(&staged_lance, None).await.map_err(|e| {
            anyhow!(
                "lance snapshot is inconsistent ({e}); the daemon likely committed mid-copy — \
                 re-run `kb backup` with the daemon paused/stopped"
            )
        })?;
        store.count_rows().await.map_err(|e| {
            anyhow!("lance snapshot failed validation ({e}); re-run with the daemon paused/stopped")
        })?;
    }

    // 3. comments — copy verbatim. Each .review/*.json is written via
    //    atomic rename, so every file is already internally consistent.
    let src_review = paths.kb_review_dir(kb_name);
    if src_review.exists() {
        copy_dir(&src_review, &staged_kb.join(".review"))?;
    }

    // 3b. SL2 — the daemon-wide `slates/` clause (design §7
    //     "Registration"). Slates are the one sidecar family in invariant
    //     #6 that is NOT per-kb: `<state>/slates/<slug>/` keys on a
    //     PROJECT, so it rides ALONGSIDE `<kb>/` in the tarball rather
    //     than inside it, and `kb restore` puts it back at the same level
    //     (the tarball is extracted into the kb-state PARENT, which IS
    //     `<state>`). Every ledger line is appended with `O_APPEND` +
    //     fsync and `meta.json` lands via atomic rename, so a file copied
    //     under a running daemon is at worst missing its newest line —
    //     never torn (`load_posts` drops an unterminated tail by design).
    //     `kb reset` still never touches slates; purge is the sole
    //     deletion path.
    let src_slates = paths.state.join("slates");
    let staged_slates = staging.join("slates");
    if src_slates.exists() {
        copy_dir(&src_slates, &staged_slates)?;
    }

    // 4. tar the staging dir by the kb basename (tarball root is `<kb>/`,
    //    matching the pre-B1 layout so existing restores keep working),
    //    plus `slates/` when this daemon has any.
    let mut cmd = Command::new("tar");
    cmd.arg("-czf")
        .arg(out_path)
        .arg("-C")
        .arg(staging)
        .arg(kb_name.as_str());
    if staged_slates.exists() {
        cmd.arg("slates");
    }
    let status = cmd
        .status()
        .with_context(|| "tar invocation failed (is `tar` installed?)")?;
    if !status.success() {
        return Err(anyhow!("tar returned exit code {status}"));
    }
    Ok(())
}

/// Recursively copy the `src` directory into `dst` (created if missing).
fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    for entry in walkdir::WalkDir::new(src) {
        let entry = entry.with_context(|| format!("walk {}", src.display()))?;
        let rel = entry
            .path()
            .strip_prefix(src)
            .expect("walkdir yields paths under src");
        let target = dst.join(rel);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&target)?;
        } else {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(entry.path(), &target).with_context(|| {
                format!("copy {} → {}", entry.path().display(), target.display())
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn outcome<'a>(kb: &'a str, error: Option<&str>) -> KbBackupOutcome<'a> {
        KbBackupOutcome {
            kb,
            error: error.map(str::to_string),
        }
    }

    #[test]
    fn all_successes_are_ok() {
        assert!(aggregate_backup_failures(&[]).is_ok());
        assert!(
            aggregate_backup_failures(&[outcome("alpha", None), outcome("beta", None)]).is_ok()
        );
    }

    #[test]
    fn fail_if_any_names_every_failed_kb() {
        let err = aggregate_backup_failures(&[
            outcome("alpha", None),
            outcome("beta", Some("no state")),
            outcome("gamma", None),
            outcome("delta", Some("lance inconsistent")),
        ])
        .expect_err("a failed kb must fail the sweep");
        let msg = err.to_string();
        assert!(msg.contains("beta"), "{msg}");
        assert!(msg.contains("delta"), "{msg}");
        assert!(msg.contains("no state"), "{msg}");
        assert!(msg.contains("lance inconsistent"), "{msg}");
        assert!(
            !msg.contains("alpha"),
            "success must not be reported as failed: {msg}"
        );
        assert!(
            !msg.contains("gamma"),
            "success must not be reported as failed: {msg}"
        );
        assert!(msg.contains("2 kb"), "{msg}");
    }

    #[test]
    fn empty_failure_text_still_fails_that_kb() {
        let err = aggregate_backup_failures(&[outcome("alpha", Some(""))])
            .expect_err("empty error text is still a failure");
        let msg = err.to_string();
        assert!(msg.contains("alpha"), "{msg}");
        assert!(msg.contains("unknown error"), "{msg}");
        assert!(msg.contains("1 kb"), "{msg}");
    }

    #[test]
    fn repeated_failure_is_not_collapsed() {
        let err = aggregate_backup_failures(&[
            outcome("beta", Some("first")),
            outcome("beta", Some("second")),
        ])
        .expect_err("both failures must count");
        let msg = err.to_string();
        assert!(msg.contains("first"), "{msg}");
        assert!(msg.contains("second"), "{msg}");
        assert!(msg.contains("2 kb"), "{msg}");
    }

    #[test]
    fn kb_names_keeps_every_named_corpus() {
        let body = json!([
            {"name": "docs", "memory_scope": null},
            {"name": "memory", "memory_scope": "global"},
            {"name": "scratch"}
        ]);
        assert_eq!(
            kb_names_from_api(&body).unwrap(),
            vec![
                "docs".to_string(),
                "memory".to_string(),
                "scratch".to_string()
            ]
        );
    }

    #[test]
    fn kb_names_rejects_a_hole_instead_of_skipping_it() {
        let hole = json!([
            {"name": "docs"},
            {"doc_count": 3},
            {"name": "scratch"}
        ]);
        let err = kb_names_from_api(&hole).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("entry 1"), "{msg}");
        assert!(msg.contains("no name"), "{msg}");

        let not_array = kb_names_from_api(&json!({"name": "docs"})).unwrap_err();
        assert!(not_array.to_string().contains("array"), "{not_array}");
    }

    #[test]
    fn backup_daemon_refuses_remote_and_allows_loopback() {
        assert!(backup_daemon_is_local(None));
        assert!(backup_daemon_is_local(Some("")));
        assert!(backup_daemon_is_local(Some("   ")));
        assert!(backup_daemon_is_local(Some("http://127.0.0.1:4000")));
        assert!(backup_daemon_is_local(Some("http://127.0.0.1:4000/")));
        assert!(backup_daemon_is_local(Some("https://localhost:4000")));
        assert!(backup_daemon_is_local(Some("http://LOCALHOST")));
        assert!(backup_daemon_is_local(Some("http://[::1]:4000")));
        assert!(backup_daemon_is_local(Some("127.0.0.1:4000")));
        assert!(backup_daemon_is_local(Some("localhost")));
        assert!(backup_daemon_is_local(Some("::1")));
        assert!(backup_daemon_is_local(Some("[::1]:4000")));
        // no host
        assert!(backup_daemon_is_local(Some("file:///tmp")));

        assert!(!backup_daemon_is_local(Some("http://example:4000")));
        assert!(!backup_daemon_is_local(Some("https://kb.example")));
        assert!(!backup_daemon_is_local(Some("example:4000")));
        assert!(!backup_daemon_is_local(Some(
            "http://127.0.0.1.example:4000"
        )));
        assert!(!backup_daemon_is_local(Some(
            "http://127.0.0.1@example:4000"
        )));
        assert!(!backup_daemon_is_local(Some("http://evil.localhost:4000")));
        assert!(!backup_daemon_is_local(Some("http://0.0.0.0:4000")));

        let line = remote_daemon_refusal("http://example:4000");
        assert!(!line.contains('\n'), "{line}");
        assert!(line.contains("http://example:4000"), "{line}");
        assert!(line.contains("refusing"), "{line}");
    }

    #[test]
    fn all_remote_dest_is_a_distinct_object_per_corpus() {
        let docs = Path::new("/state/exports/docs-20260922-120000.tar.gz");
        let memory = Path::new("/state/exports/memory-20260922-120000.tar.gz");
        let docs_dest = remote_dest_for_corpus("remote:bucket/path", docs);
        let memory_dest = remote_dest_for_corpus("remote:bucket/path", memory);
        assert_ne!(docs_dest, memory_dest);
        assert_eq!(docs_dest, "remote:bucket/path/docs-20260922-120000.tar.gz");
        assert_eq!(
            memory_dest,
            "remote:bucket/path/memory-20260922-120000.tar.gz"
        );
        assert_eq!(
            remote_dest_for_corpus("remote:bucket/path/", docs),
            "remote:bucket/path/docs-20260922-120000.tar.gz"
        );
        assert_eq!(
            remote_dest_for_corpus("user@host:/backups/", memory),
            "user@host:/backups/memory-20260922-120000.tar.gz"
        );
    }
}
