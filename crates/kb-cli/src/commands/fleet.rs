//! `kb fleet` — cross-daemon verbs over `~/.config/kb/daemons.toml`
//! (or `$KB_DAEMONS_FILE`).
//!
//! `kb fleet init --daemon name=url` writes that address book and refuses
//! to overwrite an existing file unless `--force`. `kb fleet status`
//! (v0.24 T1) reads the file when it exists instead of synthesizing a
//! loopback entry, and sweeps every entry's `/api/identity` + `/api/stats`.
//! A missing file still works; the text names the path
//! (`fleet: 1 daemon(s); daemons.toml absent`). `kb fleet doctor` GETs
//! `/healthz` on each named daemon (a missing file is a WARN, not a crash).
//! `kb fleet replicate --kb NAME` (Q4)
//! queries each daemon's `/api/kb/{kb}/docs` for the artifact id set
//! and prints a diff matrix: which daemons are missing which docs.
//!
//! Identity is the 12-hex content hash that the indexer assigns to
//! every artifact (`kb_core::ids::artifact_id`). Two daemons holding
//! the same byte-for-byte file end up with the same id, so "missing"
//! is a clean set-membership test. A doc that exists on two daemons
//! with different ids (someone edited it) shows up as one "missing"
//! row on whichever daemon doesn't have the OTHER id — same as
//! upstream rsync's `--checksum` mode.
//!
//! `--copy-to <DIR>` downloads each artifact that's missing from
//! *somewhere* (i.e. exists on at least one daemon, absent from at
//! least one other) into `<DIR>/<id>.html`. The user can then `rsync`
//! that directory into the replica's source path — a daemon-side
//! upload endpoint is deferred (see the deferred-list).
//!
//! `--src <NAME>` constrains the source to a single daemon (defaults
//! to "the first daemon that has the doc" in BTreeMap order).

use crate::http::{client_with_timeout_and_bearer, encode_path_segment};
use anyhow::{anyhow, Context, Result};
use kb_core::config::{DaemonEntry, DaemonsConfig};
use kb_core::paths::KbPaths;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Minimal DocSummary shape — we only need id + path for the diff
/// and copy-out. Other fields are discarded by `#[serde(deny_unknown_fields)]`
/// NOT being set, so the daemon can grow the schema without breaking us.
/// Shared with `kb pull`, which reuses `fetch_docs`/`fetch_artifact_bytes`.
#[derive(Debug, Deserialize)]
pub(crate) struct DocRow {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) path: String,
}

pub async fn replicate(
    kb: &str,
    src: Option<&str>,
    copy_to: Option<&Path>,
    bearer: Option<&str>,
) -> Result<()> {
    // Load daemons.toml from the kb-cli default location.
    let paths = KbPaths::new("default")?;
    let daemons = DaemonsConfig::load_or_default(&paths.daemons_file());
    if daemons.daemon.is_empty() {
        return Err(anyhow!(
            "no daemons configured — populate {}",
            paths.daemons_file().display()
        ));
    }
    if daemons.daemon.len() == 1 {
        eprintln!(
            "fleet has 1 daemon — nothing to reconcile against (configure additional entries in {})",
            paths.daemons_file().display()
        );
    }

    // Fetch the doc id set per daemon. Failures are non-fatal — a
    // single unreachable daemon shows up as `(unreachable)` in the
    // report; the rest of the fleet still gets compared. This matches
    // the TUI's per-daemon connection model.
    let client = client_with_timeout_and_bearer(10, bearer)?;
    let mut per_daemon: BTreeMap<String, DaemonState> = BTreeMap::new();
    for (name, entry) in &daemons.daemon {
        let base = entry.base_url();
        let url = format!("{base}/api/kb/{}/docs?limit=10000", encode_path_segment(kb));
        let state = match fetch_docs(&client, &url).await {
            Ok(rows) => {
                let mut id_to_path: BTreeMap<String, String> = BTreeMap::new();
                for r in rows {
                    id_to_path.insert(r.id, r.path);
                }
                DaemonState::Reachable {
                    endpoint: base.to_string(),
                    id_to_path,
                }
            }
            Err(e) => DaemonState::Unreachable {
                endpoint: base.to_string(),
                error: e.to_string(),
            },
        };
        per_daemon.insert(name.clone(), state);
    }

    // Compute the union of ids across reachable daemons + the per-
    // daemon missing sets.
    let union: BTreeSet<String> = per_daemon
        .values()
        .filter_map(|s| match s {
            DaemonState::Reachable { id_to_path, .. } => {
                Some(id_to_path.keys().cloned().collect::<Vec<_>>())
            }
            _ => None,
        })
        .flatten()
        .collect();

    if union.is_empty() {
        println!("fleet reachable but no artifacts in kb {kb} on any daemon — nothing to do");
        return Ok(());
    }

    println!("kb: {kb}");
    println!("union: {} distinct artifact id(s)", union.len());
    println!();
    println!("daemon coverage:");
    for (name, state) in &per_daemon {
        match state {
            DaemonState::Reachable {
                endpoint,
                id_to_path,
            } => {
                let missing = union.len() - id_to_path.len();
                let marker = if missing == 0 { "✓" } else { "·" };
                println!(
                    "  {marker} {name:<16} {endpoint:<36}  have={}  missing={}",
                    id_to_path.len(),
                    missing
                );
            }
            DaemonState::Unreachable { endpoint, error } => {
                println!(
                    "  ✗ {name:<16} {endpoint:<36}  unreachable: {}",
                    error.lines().next().unwrap_or(error)
                );
            }
        }
    }

    // Per-daemon missing-id list. Cap at 20 per daemon so the output
    // stays scannable; show "... and N more" for the overflow.
    println!();
    println!("missing-on:");
    let mut anything_missing = false;
    for (name, state) in &per_daemon {
        let DaemonState::Reachable { id_to_path, .. } = state else {
            continue;
        };
        let missing: Vec<String> = union
            .iter()
            .filter(|id| !id_to_path.contains_key(*id))
            .cloned()
            .collect();
        if missing.is_empty() {
            continue;
        }
        anything_missing = true;
        println!("  {name}: {} missing", missing.len());
        for id in missing.iter().take(20) {
            // Show one path hint from whichever daemon has the doc.
            let hint = first_path_for(id, &per_daemon).unwrap_or_default();
            println!("    {id}  {hint}");
        }
        if missing.len() > 20 {
            println!("    ... and {} more", missing.len() - 20);
        }
    }
    if !anything_missing {
        println!("  (all reachable daemons have the full union — fleet is in sync)");
    }

    // Optional --copy-to: download the union of missing-anywhere ids
    // into a local directory keyed by id. The operator's transport
    // (rsync, scp, manual) takes it from there.
    if let Some(dir) = copy_to {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("create --copy-to dir {}", dir.display()))?;
        let missing_anywhere: BTreeSet<String> = per_daemon
            .values()
            .filter_map(|s| match s {
                DaemonState::Reachable { id_to_path, .. } => Some(
                    union
                        .iter()
                        .filter(|id| !id_to_path.contains_key(*id))
                        .cloned()
                        .collect::<Vec<_>>(),
                ),
                _ => None,
            })
            .flatten()
            .collect();
        if missing_anywhere.is_empty() {
            println!("\n--copy-to: nothing to copy (fleet is in sync)");
            return Ok(());
        }
        println!(
            "\ncopying {} artifact(s) into {} …",
            missing_anywhere.len(),
            dir.display()
        );
        let src_pin = src.map(str::to_string);
        let mut copied = 0u32;
        let mut failed = 0u32;
        for id in &missing_anywhere {
            let source = match pick_source(id, src_pin.as_deref(), &per_daemon) {
                Some(s) => s,
                None => {
                    eprintln!("  skip {id}: no reachable daemon holds it");
                    failed += 1;
                    continue;
                }
            };
            let url = format!(
                "{}/api/kb/{}/artifact/{id}",
                source,
                encode_path_segment(kb)
            );
            match fetch_artifact_bytes(&client, &url).await {
                Ok(bytes) => {
                    let dest = dir.join(format!("{id}.html"));
                    std::fs::write(&dest, &bytes).with_context(|| {
                        format!("write {} ({} bytes)", dest.display(), bytes.len())
                    })?;
                    copied += 1;
                }
                Err(e) => {
                    eprintln!("  fail {id} from {source}: {e}");
                    failed += 1;
                }
            }
        }
        println!("--copy-to: copied {copied}, failed {failed}");
    }
    Ok(())
}

enum DaemonState {
    Reachable {
        endpoint: String,
        id_to_path: BTreeMap<String, String>,
    },
    Unreachable {
        endpoint: String,
        error: String,
    },
}

// ---- address book -------------------------------------------------------

/// Address book path. `$KB_DAEMONS_FILE` wins when set and non-empty;
/// otherwise `~/.config/kb/daemons.toml` (honours `KB_CONFIG_DIR` /
/// `KB_HOME` via [`KbPaths`]).
fn address_book_path() -> Result<PathBuf> {
    if let Some(raw) = std::env::var_os("KB_DAEMONS_FILE") {
        if !raw.is_empty() {
            return Ok(PathBuf::from(raw));
        }
    }
    Ok(KbPaths::new("default")?.daemons_file())
}

/// Human status header. A missing file still reports the synthesized
/// loopback count and names the path
/// (`fleet: 1 daemon(s); daemons.toml absent`).
fn status_header(count: usize, path: &Path, absent: bool) -> String {
    if absent {
        format!(
            "fleet: {count} daemon(s); daemons.toml absent ({})",
            path.display()
        )
    } else {
        format!("fleet: {count} daemon(s) ({})", path.display())
    }
}

fn missing_book_warn(path: &Path) -> String {
    format!("WARN: daemons.toml absent ({})", path.display())
}

// ---- `kb fleet init` ----------------------------------------------------

/// One `--daemon name=url` flag that did not parse.
#[derive(Debug, PartialEq, Eq)]
struct DaemonFlagError {
    raw: String,
    reason: &'static str,
}

/// `kb fleet init --daemon name=url [--daemon ...] [--force]`.
///
/// Writes the named daemons to `~/.config/kb/daemons.toml` (or
/// `$KB_DAEMONS_FILE`). Refuses to overwrite an existing file unless
/// `force`. Does not synthesize a loopback entry the operator did not
/// name. Prints the path.
///
/// Registered from `FleetAction` in `main.rs` — this module does not
/// own the clap enum.
pub async fn init(daemon_flags: &[String], force: bool) -> Result<()> {
    init_at(&address_book_path()?, daemon_flags, force)
}

/// Write `daemon_flags` to `path`. Flags are parsed before the file is
/// touched, so a bad invocation cannot clobber an existing book.
fn init_at(path: &Path, daemon_flags: &[String], force: bool) -> Result<()> {
    let cfg = match daemons_from_flags(daemon_flags) {
        Ok(cfg) => cfg,
        Err(e) if e.raw.is_empty() => {
            return Err(anyhow!(
                "no daemons named — pass --daemon name=url (repeatable)"
            ));
        }
        Err(e) => {
            return Err(anyhow!(
                "invalid --daemon '{}' — {}; expected name=url",
                e.raw,
                e.reason
            ));
        }
    };
    if path.exists() && !force {
        return Err(anyhow!(
            "refusing to overwrite {} — pass --force",
            path.display()
        ));
    }
    let body = render_daemons_toml(&cfg);
    kb_core::fsx::write_atomic(path, body.as_bytes())
        .map_err(|e| anyhow!("write {}: {e}", path.display()))?;
    println!("{}", path.display());
    Ok(())
}

/// Parse repeated `name=url` flags into an address book. Empty input
/// and malformed flags are errors. Duplicate names are errors. No
/// loopback entry is added.
fn daemons_from_flags(flags: &[String]) -> std::result::Result<DaemonsConfig, DaemonFlagError> {
    if flags.is_empty() {
        return Err(DaemonFlagError {
            raw: String::new(),
            reason: "at least one --daemon name=url is required",
        });
    }
    let mut cfg = DaemonsConfig::default();
    for raw in flags {
        let (name, url) = split_daemon_flag(raw)?;
        if cfg.daemon.contains_key(&name) {
            return Err(DaemonFlagError {
                raw: raw.clone(),
                reason: "duplicate daemon name",
            });
        }
        cfg.daemon.insert(name, DaemonEntry { endpoint: url });
    }
    Ok(cfg)
}

fn split_daemon_flag(raw: &str) -> std::result::Result<(String, String), DaemonFlagError> {
    let fail = |reason: &'static str| DaemonFlagError {
        raw: raw.to_string(),
        reason,
    };
    let Some((name, url)) = raw.split_once('=') else {
        return Err(fail("missing '='"));
    };
    let name = name.trim();
    let url = url.trim();
    if name.is_empty() {
        return Err(fail("empty name"));
    }
    if url.is_empty() {
        return Err(fail("empty url"));
    }
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(fail("url must start with http:// or https://"));
    }
    Ok((name.to_string(), url.to_string()))
}

fn render_daemons_toml(cfg: &DaemonsConfig) -> String {
    let mut out = String::from(
        "# kb fleet address book — one [daemon.<name>] per host.\n\
         # Written by `kb fleet init --daemon name=url`.\n",
    );
    for (name, entry) in &cfg.daemon {
        out.push_str(&format!(
            "\n[daemon.{}]\nendpoint = {}\n",
            toml_key(name),
            toml_basic_string(&entry.endpoint),
        ));
    }
    out
}

fn toml_key(s: &str) -> String {
    let bare = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if bare {
        s.to_string()
    } else {
        toml_basic_string(s)
    }
}

fn toml_basic_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' | '"' => {
                out.push('\\');
                out.push(c);
            }
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ---- `kb fleet status` (v0.24 T1) --------------------------------------

/// Cross-daemon health sweep — the TUI fleet grid's replacement. For
/// every daemons.toml entry: `GET /api/identity` (who are you, which
/// kbs) + `GET /api/stats` (docs + open errors per kb). Per-daemon
/// failures are non-fatal (an unreachable daemon is a report row, not
/// an abort) — the same connection model as `replicate` above.
///
/// A file that exists is the fleet: it is not padded with a synthesized
/// loopback entry. A missing file still sweeps the local default so
/// status works, and the text names the path
/// (`fleet: 1 daemon(s); daemons.toml absent`).
pub async fn status(json: bool, bearer: Option<&str>) -> Result<()> {
    let daemons_file = address_book_path()?;
    let (daemons, absent) = if daemons_file.exists() {
        (
            DaemonsConfig::load(&daemons_file)
                .map_err(|e| anyhow!("read {}: {e}", daemons_file.display()))?,
            false,
        )
    } else {
        (DaemonsConfig::default_local(), true)
    };
    if daemons.daemon.is_empty() {
        return Err(anyhow!(
            "no daemons configured — populate {}",
            daemons_file.display()
        ));
    }

    let client = client_with_timeout_and_bearer(10, bearer)?;
    let mut rows: Vec<serde_json::Value> = Vec::new();
    for (name, entry) in &daemons.daemon {
        let base = entry.base_url();
        let row = match fetch_status(&client, base).await {
            Ok((identity, stats)) => serde_json::json!({
                "name": name,
                "endpoint": base,
                "reachable": true,
                "identity": identity,
                "stats": stats,
            }),
            Err(e) => serde_json::json!({
                "name": name,
                "endpoint": base,
                "reachable": false,
                "error": e.to_string(),
            }),
        };
        rows.push(row);
    }

    let header = status_header(rows.len(), &daemons_file, absent);
    if json {
        if absent {
            eprintln!("{header}");
        }
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    println!("{header}");
    for row in &rows {
        print_status_row(row);
    }
    Ok(())
}

/// Identity + stats for one daemon. Values stay `serde_json::Value` so
/// a daemon running an older/newer build can't break the sweep — we
/// read the fields we know and pass the rest through in `--json`.
async fn fetch_status(
    client: &reqwest::Client,
    base: &str,
) -> Result<(serde_json::Value, serde_json::Value)> {
    let fetch = |path: &'static str| async move {
        let url = format!("{base}/api{path}");
        let resp = client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?
            .error_for_status()
            .with_context(|| format!("GET {url}"))?;
        anyhow::Ok(resp.json::<serde_json::Value>().await?)
    };
    let identity = fetch("/identity").await?;
    let stats = fetch("/stats").await?;
    Ok((identity, stats))
}

fn print_status_row(row: &serde_json::Value) {
    let s = |v: &serde_json::Value, key: &str| -> String {
        v.get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?")
            .to_string()
    };
    let name = s(row, "name");
    let endpoint = s(row, "endpoint");
    if !row["reachable"].as_bool().unwrap_or(false) {
        let error = s(row, "error");
        println!(
            "  ✗ {name:<16} {endpoint:<36}  unreachable: {}",
            error.lines().next().unwrap_or(&error)
        );
        return;
    }
    let identity = &row["identity"];
    let stats = &row["stats"];
    println!(
        "  ✓ {name:<16} {endpoint:<36}  {} ({})  up since {}  docs={} open-errors={}",
        s(identity, "version"),
        s(identity, "build_sha"),
        s(identity, "started_at"),
        stats["total_docs"].as_u64().unwrap_or(0),
        stats["total_open_errors"].as_u64().unwrap_or(0),
    );
    if let Some(kbs) = stats["kbs"].as_array() {
        for kb in kbs {
            println!(
                "      {:<20} docs={:<6} errors={}",
                s(kb, "name"),
                kb["doc_count"].as_u64().unwrap_or(0),
                kb["open_errors"].as_u64().unwrap_or(0),
            );
        }
    }
}

// ---- `kb fleet doctor` --------------------------------------------------

/// `kb fleet doctor` — `GET /healthz` on each named daemon.
///
/// Prints `ok <name>` or `fail <name> <reason>` per entry. A missing
/// address book is a WARN on stderr and a success return — not a crash.
/// Unreachable daemons are fail rows, not an abort of the sweep.
pub async fn doctor(bearer: Option<&str>) -> Result<()> {
    let path = address_book_path()?;
    if !path.exists() {
        eprintln!("{}", missing_book_warn(&path));
        return Ok(());
    }
    let daemons =
        DaemonsConfig::load(&path).map_err(|e| anyhow!("read {}: {e}", path.display()))?;
    if daemons.daemon.is_empty() {
        eprintln!("WARN: {} names no daemons", path.display());
        return Ok(());
    }
    let client = client_with_timeout_and_bearer(5, bearer)?;
    for (name, entry) in &daemons.daemon {
        let url = format!("{}/healthz", entry.base_url());
        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => println!("ok {name}"),
            Ok(resp) => println!("fail {name} HTTP {}", resp.status()),
            Err(e) => {
                let reason = e.to_string();
                let first = reason.lines().next().unwrap_or(&reason);
                println!("fail {name} {first}");
            }
        }
    }
    Ok(())
}

pub(crate) async fn fetch_docs(client: &reqwest::Client, url: &str) -> Result<Vec<DocRow>> {
    let resp = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("GET {url}"))?;
    let rows: Vec<DocRow> = resp.json().await?;
    Ok(rows)
}

pub(crate) async fn fetch_artifact_bytes(client: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    let resp = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("GET {url}"))?;
    Ok(resp.bytes().await?.to_vec())
}

fn first_path_for(id: &str, per_daemon: &BTreeMap<String, DaemonState>) -> Option<String> {
    for state in per_daemon.values() {
        if let DaemonState::Reachable { id_to_path, .. } = state {
            if let Some(p) = id_to_path.get(id) {
                if !p.is_empty() {
                    return Some(p.clone());
                }
            }
        }
    }
    None
}

/// Pick a source daemon's endpoint that holds `id`. With `--src NAME`
/// set, only that daemon counts; otherwise the first reachable daemon
/// (BTreeMap order) that has the doc wins.
fn pick_source(
    id: &str,
    src_name: Option<&str>,
    per_daemon: &BTreeMap<String, DaemonState>,
) -> Option<String> {
    if let Some(name) = src_name {
        if let Some(DaemonState::Reachable {
            endpoint,
            id_to_path,
        }) = per_daemon.get(name)
        {
            if id_to_path.contains_key(id) {
                return Some(endpoint.clone());
            }
        }
        return None;
    }
    for state in per_daemon.values() {
        if let DaemonState::Reachable {
            endpoint,
            id_to_path,
        } = state
        {
            if id_to_path.contains_key(id) {
                return Some(endpoint.clone());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{
        daemons_from_flags, init_at, missing_book_warn, render_daemons_toml, status_header,
    };
    use kb_core::config::DaemonsConfig;
    use std::path::Path;

    #[test]
    fn absent_status_header_names_the_missing_path() {
        let path = Path::new("/home/nik/.config/kb/daemons.toml");
        let msg = status_header(1, path, true);
        assert!(
            msg.contains("fleet: 1 daemon(s); daemons.toml absent"),
            "{msg}"
        );
        assert!(msg.contains(&path.display().to_string()), "{msg}");
    }

    #[test]
    fn present_status_header_does_not_claim_absent() {
        let path = Path::new("/tmp/daemons.toml");
        let msg = status_header(2, path, false);
        assert_eq!(msg, "fleet: 2 daemon(s) (/tmp/daemons.toml)");
        assert!(!msg.contains("absent"), "{msg}");
    }

    #[test]
    fn missing_book_warn_names_the_path() {
        let path = Path::new("/home/nik/.config/kb/daemons.toml");
        let msg = missing_book_warn(path);
        assert!(msg.starts_with("WARN:"), "{msg}");
        assert!(msg.contains("daemons.toml absent"), "{msg}");
        assert!(msg.contains(&path.display().to_string()), "{msg}");
    }

    #[test]
    fn daemons_from_flags_records_named_endpoints_only() {
        let cfg = daemons_from_flags(&[
            "h=https://kb.example/x?a=1".into(),
            " local = http://127.0.0.1:4000 ".into(),
        ])
        .unwrap();
        assert_eq!(cfg.daemon.len(), 2);
        assert_eq!(cfg.daemon["h"].endpoint, "https://kb.example/x?a=1");
        assert_eq!(cfg.daemon["local"].endpoint, "http://127.0.0.1:4000");
        let body = render_daemons_toml(&cfg);
        assert!(body.contains("[daemon.h]"), "{body}");
        assert!(body.contains("https://kb.example/x?a=1"), "{body}");
    }

    #[test]
    fn daemons_from_flags_rejects_empty_malformed_and_duplicates() {
        assert!(daemons_from_flags(&[]).is_err());
        assert!(daemons_from_flags(&["local".into()]).is_err());
        assert!(daemons_from_flags(&["=http://127.0.0.1:4000".into()]).is_err());
        assert!(daemons_from_flags(&["local=".into()]).is_err());
        assert!(daemons_from_flags(&["local=127.0.0.1:4000".into()]).is_err());
        assert!(daemons_from_flags(&[
            "h=https://kb.example".into(),
            "h=https://other.example".into(),
        ])
        .is_err());
    }

    #[test]
    fn init_at_writes_and_refuses_to_clobber_without_force() {
        let dir = std::env::temp_dir().join(format!(
            "kb-fleet-init-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("daemons.toml");

        init_at(&path, &["local=http://127.0.0.1:4000".into()], false).unwrap();
        let written = DaemonsConfig::load(&path).unwrap();
        assert_eq!(written.daemon["local"].endpoint, "http://127.0.0.1:4000");
        assert!(!path_body(&path).contains("[daemon.h]"));

        let refused = init_at(&path, &["h=https://kb.example".into()], false).unwrap_err();
        assert!(
            refused.to_string().contains("refusing to overwrite"),
            "{refused}"
        );
        let still = DaemonsConfig::load(&path).unwrap();
        assert_eq!(still.daemon.len(), 1);
        assert_eq!(still.daemon["local"].endpoint, "http://127.0.0.1:4000");

        let bad = init_at(&path, &["not-a-flag".into()], true).unwrap_err();
        assert!(bad.to_string().contains("invalid --daemon"), "{bad}");
        assert_eq!(
            DaemonsConfig::load(&path).unwrap().daemon["local"].endpoint,
            "http://127.0.0.1:4000",
            "a bad --force invocation must not clobber"
        );

        init_at(&path, &["h=https://kb.example".into()], true).unwrap();
        let forced = DaemonsConfig::load(&path).unwrap();
        assert_eq!(forced.daemon.len(), 1, "force replaces the book");
        assert_eq!(forced.daemon["h"].endpoint, "https://kb.example");
        assert!(!forced.daemon.contains_key("local"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn path_body(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }
}
