//! `kb fleet` — cross-daemon verbs over `~/.config/kb/daemons.toml`.
//!
//! `kb fleet init` writes that address book with the local daemon and
//! leaves existing entries intact. A file that already has entries is
//! not overwritten unless `--force`. `kb fleet status` (v0.24 T1)
//! sweeps every configured daemon's `/api/identity` + `/api/stats`
//! into one health report (the TUI fleet grid's CLI replacement).
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
use kb_core::config::DaemonsConfig;
use kb_core::paths::KbPaths;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

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

// ---- `kb fleet init` ----------------------------------------------------

/// Refused because the address book already has entries and `--force`
/// was not passed. The file is left untouched.
#[derive(Debug, PartialEq, Eq)]
struct InitRefused {
    entries: usize,
}

/// `kb fleet init [--force]`.
///
/// Writes `~/.config/kb/daemons.toml` (honours `KB_CONFIG_DIR` /
/// `KB_HOME`) with the local daemon (`local` → `http://127.0.0.1:4000`).
/// Existing entries are kept. A file that already has entries is not
/// rewritten unless `force`; even then peers are not removed and an
/// existing `local` endpoint is not replaced. Prints the path.
///
/// Registered from `FleetAction` in `main.rs` — this module does not
/// own the clap enum.
pub async fn init(force: bool) -> Result<()> {
    let paths = KbPaths::new("default")?;
    let path = paths.daemons_file();
    let existing = if path.exists() {
        Some(
            DaemonsConfig::load(&path)
                .map_err(|e| anyhow!("read {}: {e}", path.display()))?,
        )
    } else {
        None
    };
    let cfg = match decide_init(existing.as_ref(), force) {
        Ok(cfg) => cfg,
        Err(refused) => {
            let noun = if refused.entries == 1 {
                "entry"
            } else {
                "entries"
            };
            return Err(anyhow!(
                "refusing to clobber {} — address book already has {} {noun}; pass --force to add the local daemon without removing existing entries",
                path.display(),
                refused.entries,
            ));
        }
    };
    let body = render_daemons_toml(&cfg);
    kb_core::fsx::write_atomic(&path, body.as_bytes())
        .map_err(|e| anyhow!("write {}: {e}", path.display()))?;
    println!("{}", path.display());
    Ok(())
}

/// Seed a missing or empty address book with the local daemon. Refuse
/// to rewrite a book that already has entries unless `force`. On
/// force, insert `local` only when absent — never replace a peer or
/// an existing local endpoint.
fn decide_init(
    existing: Option<&DaemonsConfig>,
    force: bool,
) -> std::result::Result<DaemonsConfig, InitRefused> {
    match existing {
        None => Ok(DaemonsConfig::default_local()),
        Some(cfg) if cfg.daemon.is_empty() => Ok(DaemonsConfig::default_local()),
        Some(cfg) if !force => Err(InitRefused {
            entries: cfg.daemon.len(),
        }),
        Some(cfg) => Ok(ensure_local(cfg)),
    }
}

fn ensure_local(cfg: &DaemonsConfig) -> DaemonsConfig {
    let mut out = cfg.clone();
    if let Some((name, entry)) = DaemonsConfig::default_local().daemon.into_iter().next() {
        out.daemon.entry(name).or_insert(entry);
    }
    out
}

fn absent_address_book_message(path: &Path) -> String {
    format!(
        "fleet: address book absent ({}); this is not a configured fleet. \
         Run `kb fleet init` to record the local daemon.",
        path.display()
    )
}

fn render_daemons_toml(cfg: &DaemonsConfig) -> String {
    let mut out = String::from(
        "# kb fleet address book — one [daemon.<name>] per host.\n\
         # `kb fleet init` records the local daemon and does not remove peers.\n",
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
/// A missing address book is reported as absent, not swept as one
/// daemon. A file that exists and lists one daemon is still that one
/// daemon.
pub async fn status(json: bool, bearer: Option<&str>) -> Result<()> {
    let paths = KbPaths::new("default")?;
    let daemons_file = paths.daemons_file();
    // Missing daemons.toml is not a one-daemon fleet. load_or_default
    // used to synthesise `local`, so status printed "fleet: 1 daemon(s)"
    // on every host that had never written the address book.
    if !daemons_file.exists() {
        let message = absent_address_book_message(&daemons_file);
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "address_book": "absent",
                    "path": daemons_file.display().to_string(),
                    "message": message,
                }))?
            );
        } else {
            println!("{message}");
        }
        return Ok(());
    }
    let daemons = DaemonsConfig::load(&daemons_file)
        .map_err(|e| anyhow!("read {}: {e}", daemons_file.display()))?;
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

    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    println!(
        "fleet: {} daemon(s) ({})",
        rows.len(),
        daemons_file.display()
    );
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
    use super::{absent_address_book_message, decide_init};
    use kb_core::config::{DaemonEntry, DaemonsConfig};
    use std::path::Path;

    #[test]
    fn absent_address_book_message_does_not_claim_one_daemon() {
        let path = Path::new("/home/nik/.config/kb/daemons.toml");
        let msg = absent_address_book_message(path);
        assert!(msg.contains("address book"), "{msg}");
        assert!(msg.contains("absent"), "{msg}");
        assert!(msg.contains(&path.display().to_string()), "{msg}");
        assert!(
            !msg.contains("daemon(s)"),
            "missing file must not read as a healthy fleet count: {msg}"
        );
    }

    #[test]
    fn decide_init_does_not_clobber_existing_entries() {
        let mut existing = DaemonsConfig::default_local();
        existing.daemon.get_mut("local").unwrap().endpoint = "http://127.0.0.1:9999".into();
        existing.daemon.insert(
            "h".into(),
            DaemonEntry {
                endpoint: "https://kb.example".into(),
            },
        );

        let refused = decide_init(Some(&existing), false).expect_err("must not clobber");
        assert_eq!(refused.entries, 2);
        assert_eq!(existing.daemon["local"].endpoint, "http://127.0.0.1:9999");
        assert_eq!(existing.daemon["h"].endpoint, "https://kb.example");

        let written = decide_init(Some(&existing), true).unwrap();
        assert_eq!(written.daemon.len(), 2, "force must not drop peers");
        assert_eq!(
            written.daemon["local"].endpoint, "http://127.0.0.1:9999",
            "force must not replace an existing local endpoint"
        );
        assert_eq!(written.daemon["h"].endpoint, "https://kb.example");
    }

    #[test]
    fn decide_init_seeds_local_when_address_book_missing() {
        let cfg = decide_init(None, false).unwrap();
        assert_eq!(cfg.daemon.len(), 1);
        assert_eq!(cfg.daemon["local"].endpoint, "http://127.0.0.1:4000");
    }
}
