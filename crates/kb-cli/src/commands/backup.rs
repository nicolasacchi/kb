//! `kb backup <kb> [--out PATH]` — write a CONSISTENT tarball snapshot of
//! a kb's persistent state under `<state>/exports/`: every persistent
//! per-kb member of `KbPaths::state_members` (`index.db`, `lance/`,
//! `.review/`, `.attachments/`, `.proposals/`) plus the daemon-scope
//! members (`slates/`, `saved-queries.json`, `memory-policy.json`,
//! `tombstone-era.json`).
//!
//! The tarball is written by `kb_core::storage::backup::write_kb_export_with`
//! — the SAME writer the daemon's `[backup] schedule_hours` task uses. This
//! module keeps no packing logic of its own (it once had a twin that
//! diverged: no `.attachments`/`.proposals`, and a failed `tar` left a
//! truncated file under the real name).
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
//! When `[backup]` off-host copy is configured, `--all` (and the daemon
//! schedule) append each tarball's basename to `remote_dest`
//! (`BackupSection::for_tarball`). A shared `{dest}` (`rclone copyto`)
//! would otherwise keep only the last corpus. A single `kb backup <kb>`
//! passes `remote_dest` verbatim.

use super::{load_config_or_default, resolve_config_path};
use crate::http::client_with_timeout_and_bearer;
use anyhow::{anyhow, Context, Result};
use kb_core::paths::KbPaths;
use kb_core::types::KbName;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub async fn run(config_path: Option<&PathBuf>, kb: &str, out: Option<&Path>) -> Result<()> {
    snapshot(config_path, kb, out, OffHostDest::Configured, true).await
}

async fn snapshot(
    config_path: Option<&PathBuf>,
    kb: &str,
    out: Option<&Path>,
    off_host: OffHostDest,
    include_daemon: bool,
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

    // One writer for every backup path: stages under exports/, tars to a
    // `.partial`, renames on success, and cleans up on every outcome.
    let out_path = kb_core::storage::backup::write_kb_export_with(
        &paths,
        &kb_name,
        &kb_core::storage::backup::ExportOptions {
            include_daemon,
            out: out.map(PathBuf::from),
        },
    )
    .await
    .map_err(|e| anyhow!("{e}"))?;

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
    let backup_cfg = if matches!(off_host, OffHostDest::PerCorpus) {
        cfg.backup.for_tarball(&out_path)
    } else {
        cfg.backup.clone()
    };
    let shown_dest = backup_cfg.remote_dest.as_deref().unwrap_or("?");
    match kb_core::storage::backup::run_remote_copy(&backup_cfg, &out_path) {
        None => {}
        Some(kb_core::storage::backup::RemoteCopyOutcome::Ok) => {
            let _ = kb_core::storage::backup::mark_uploaded(&out_path);
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
    // The same kb the daemon schedule picks (first in NAME order, not in
    // `GET /api/kbs` response order).
    let daemon_scope = kb_core::storage::backup::daemon_scope_kb(names.iter().map(String::as_str));
    for kb in names.iter() {
        // Do not `?` here. A failed kb must be recorded, and every later kb
        // must still be backed up.
        // Daemon-scope state (slates, daemon JSON) rides in the FIRST kb's
        // tarball only, as in the daemon's schedule.
        let error = match snapshot(
            config_path,
            kb,
            None,
            OffHostDest::PerCorpus,
            daemon_scope == Some(kb.as_str()),
        )
        .await
        {
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

async fn list_kb_names(daemon: Option<&str>, bearer: Option<&str>) -> Result<Vec<String>> {
    let base = daemon.unwrap_or(DEFAULT_DAEMON).trim_end_matches('/');
    let url = format!("{base}{}", crate::commands::memory::KBS_CONFIG_PATH);
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

#[cfg(test)]
mod tests {

    /// v044-X3 — `backup --all` needs the corpus NAMES only; it must ask for
    /// the config-only listing, not the per-corpus row-count fan-out.
    #[tokio::test]
    async fn list_kb_names_asks_for_the_config_only_listing() {
        let (url, rx) = crate::http::tests::recording_stub(r#"[{"name":"a"},{"name":"b"}]"#);
        let names = list_kb_names(Some(&url), None).await.unwrap();
        assert_eq!(names, vec!["a".to_string(), "b".to_string()]);
        let line = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert_eq!(line, "GET /api/kbs?counts=false HTTP/1.1");
    }
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
        let cfg = kb_core::config::BackupSection {
            remote_cmd: Some(vec![
                "rclone".into(),
                "copyto".into(),
                "{src}".into(),
                "{dest}".into(),
            ]),
            remote_dest: Some("remote:bucket/path".into()),
            ..Default::default()
        };
        let docs = Path::new("/state/exports/docs-20260922-120000.tar.gz");
        let memory = Path::new("/state/exports/memory-20260922-120000.tar.gz");
        let docs_dest = cfg.for_tarball(docs).remote_dest.unwrap();
        let memory_dest = cfg.for_tarball(memory).remote_dest.unwrap();
        assert_ne!(docs_dest, memory_dest);
        assert_eq!(docs_dest, "remote:bucket/path/docs-20260922-120000.tar.gz");
        assert_eq!(
            memory_dest,
            "remote:bucket/path/memory-20260922-120000.tar.gz"
        );
    }
}
