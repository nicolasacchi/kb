//! `kb model {list, download, set, rm}` — embedding-model lifecycle CLI
//! per topic 02 §Decisions.
//!
//! Storage layout (per spike-fastembed finding 5):
//!   <XDG_CACHE_HOME>/kb/models/<model-name>/<hf-revision>/
//! The `kb_core::embed` module owns the layout; this CLI is a thin wrapper
//! around `Embedder::new` (which triggers fastembed's hf-hub download on
//! first use) plus simple file-system bookkeeping.

use super::{load_config_or_default, resolve_config_path};
use anyhow::{anyhow, bail, Result};
use kb_core::embed::{model_info, models_cache_dir, SUPPORTED_MODELS};
use kb_core::paths::KbPaths;
use kb_core::types::KbName;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub async fn run(args: ModelArgs, config_path: Option<&PathBuf>) -> Result<()> {
    match args {
        ModelArgs::List => list_models(config_path),
        ModelArgs::Download { name, from } => download_model(&name, from.as_deref()).await,
        ModelArgs::Set { name, kb, in_place } => {
            set_model(config_path, &name, kb.as_deref(), in_place).await
        }
        ModelArgs::Rm { name, force } => remove_model(config_path, &name, force),
    }
}

#[derive(Debug, Clone)]
pub enum ModelArgs {
    List,
    Download {
        name: String,
        from: Option<PathBuf>,
    },
    Set {
        name: String,
        kb: Option<String>,
        in_place: bool,
    },
    Rm {
        name: String,
        force: bool,
    },
}

fn cache_dir() -> Result<PathBuf> {
    let paths = KbPaths::new("default")?;
    Ok(models_cache_dir(&paths.cache))
}

/// Locate `model_name` in fastembed's on-disk cache. fastembed 5 stores
/// models under the HF-hub layout (`models--<org>--<name>`); a flat
/// `<name>` directory is also accepted (forward-compat / pre-seeded
/// caches like the Docker image's). Returns the cache subdir if present.
fn cached_model_dir(cache: &Path, model_name: &str) -> Option<PathBuf> {
    let flat = cache.join(model_name);
    if flat.exists() {
        return Some(flat);
    }
    let suffix = format!("--{model_name}");
    std::fs::read_dir(cache).ok()?.flatten().find_map(|e| {
        let n = e.file_name();
        let n = n.to_string_lossy();
        (n.starts_with("models--") && n.ends_with(suffix.as_str())).then(|| e.path())
    })
}

/// D3 — render the "daemon default" line shown above the `kb model list`
/// table. Pure function on the `[defaults]` block so the formatting is
/// unit-testable. Three states surfaced:
///
/// - `[defaults] embedding_model = "name"` → "from [defaults]"
/// - `disable_embedder_fallback = true` (no `embedding_model`) → "(none —
///   disabled)"
/// - neither set → "registry fallback" (currently `bge-small-en-v1.5`)
fn daemon_default_line(defaults: &kb_core::config::DefaultsSection) -> String {
    if let Some(name) = defaults.embedding_model.as_deref() {
        return match model_info(name) {
            Some(info) => format!(
                "daemon default: {} ({}-d, from [defaults])",
                info.name, info.dim
            ),
            None => format!(
                "daemon default: {name:?} (unknown — will fall through to registry default)"
            ),
        };
    }
    if defaults.disable_embedder_fallback {
        return "daemon default: (none — disable_embedder_fallback set; \
                semantic/hybrid disabled for kbs without an explicit model)"
            .to_string();
    }
    match SUPPORTED_MODELS.iter().find(|m| m.default) {
        Some(info) => format!(
            "daemon default: {} ({}-d, registry fallback)",
            info.name, info.dim
        ),
        None => "daemon default: (none — no registry default registered)".to_string(),
    }
}

fn list_models(config_path: Option<&PathBuf>) -> Result<()> {
    let cfg_path = resolve_config_path(config_path)?;
    let cfg = load_config_or_default(&cfg_path)?;
    // Map from model name → list of kbs that reference it. Pre-fix
    // (deep-review M-cli) this rendered USED-BY as literal "default
    // kb" whenever ANY kb used the model, regardless of which kb.
    let mut kbs_by_model: std::collections::BTreeMap<&str, Vec<&str>> =
        std::collections::BTreeMap::new();
    for (name, section) in &cfg.kb {
        if let Some(model) = section.embedding_model.as_deref() {
            kbs_by_model.entry(model).or_default().push(name.as_str());
        }
    }

    let cache = cache_dir()?;
    println!("{}", daemon_default_line(&cfg.defaults));
    println!();

    println!(
        "{:<32} {:<8} {:<10} {:<10} USED-BY",
        "MODEL", "DIM", "LICENSE", "STATUS"
    );
    for m in SUPPORTED_MODELS {
        let status = if cached_model_dir(&cache, m.name).is_some() {
            "downloaded"
        } else {
            "(remote)"
        };
        let used_by = match kbs_by_model.get(m.name) {
            Some(kbs) => kbs.join(","),
            None => "-".to_string(),
        };
        println!(
            "{:<32} {:<8} {:<10} {:<10} {}",
            m.name, m.dim, m.license, status, used_by
        );
    }
    Ok(())
}

async fn download_model(name: &str, from: Option<&std::path::Path>) -> Result<()> {
    let info =
        model_info(name).ok_or_else(|| anyhow!("unknown model: {name:?}; try `kb model list`"))?;
    let cache = cache_dir()?;
    std::fs::create_dir_all(&cache)?;

    if let Some(_path) = from {
        // v0.2: extract a pre-downloaded tarball. v0.1 prints a clear note.
        anyhow::bail!(
            "--from <PATH> (air-gapped install) not implemented in v0.1; \
             remove the flag and let fastembed download from HF."
        );
    }

    eprintln!(
        "downloading {} ({} MB approx) to {} ...",
        info.name,
        info.approx_size_mb,
        cache.display()
    );
    // kb-cli links no ONNX — delegate the actual download to the kb-embedder
    // binary (`--download-only` loads the model into the cache, then exits).
    let bin = kb_core::embed_ipc::locate_embedder_bin()
        .map_err(|e| anyhow!("`kb model download` needs the kb-embedder binary: {e}"))?;
    let status = std::process::Command::new(&bin)
        .arg("--model")
        .arg(name)
        .arg("--cache")
        .arg(&cache)
        .arg("--download-only")
        .status()
        .map_err(|e| anyhow!("spawn {}: {e}", bin.display()))?;
    if !status.success() {
        bail!("failed to download {name} (kb-embedder exited with {status})");
    }
    println!("✓ {name} ready at {}", cache.display());
    Ok(())
}

async fn set_model(
    config_path: Option<&PathBuf>,
    name: &str,
    kb: Option<&str>,
    in_place: bool,
) -> Result<()> {
    let info =
        model_info(name).ok_or_else(|| anyhow!("unknown model: {name:?}; try `kb model list`"))?;
    let cfg_path = resolve_config_path(config_path)?;
    let mut cfg = load_config_or_default(&cfg_path)?;

    let target_kb = match kb {
        Some(k) => KbName::new(k).map_err(|e| anyhow!("invalid kb {k:?}: {e}"))?,
        None if cfg.kb.len() == 1 => cfg.kb.keys().next().unwrap().clone(),
        None => {
            return Err(anyhow!(
                "must specify --kb when multiple kbs are configured (or none)"
            ))
        }
    };

    let section = cfg
        .kb
        .get_mut(&target_kb)
        .ok_or_else(|| anyhow!("kb {target_kb} not found in {}", cfg_path.display()))?;

    let old_dim = section
        .embedding_model
        .as_ref()
        .and_then(|m| model_info(m))
        .map(|i| i.dim);

    // Warn on dimension change — full reindex required.
    if let Some(old) = &section.embedding_model {
        if let Some(old_info) = model_info(old) {
            if old_info.dim != info.dim {
                eprintln!(
                    "warning: changing model {} ({}-dim) → {} ({}-dim).\n\
                     all docs in this kb need re-embedding on next index run.",
                    old, old_info.dim, info.name, info.dim
                );
                if in_place {
                    return Err(anyhow!(
                        "--in-place requires the new model to have the same embedding dim. \
                         A different-dim swap means the lance dataset's `embedding` column width \
                         differs from what the new model emits — `Storage::open` will refuse \
                         to reopen this kb on the daemon side, and an in-place NULL-out wouldn't \
                         help. Drop --in-place, accept the full reindex (stand up a fresh kb \
                         against the same source dir, or wipe `<state>/<daemon>/{target_kb}/lance` \
                         first), then run `kb daemon`."
                    ));
                }
            }
        }
    }

    section.embedding_model = Some(name.to_string());
    cfg.save(&cfg_path)?;

    // v0.3 G4 — for same-dim swaps, NULL the embedding column
    // in-place. This lets the indexer's content-hash gate skip
    // already-embedded docs while forcing a re-embed for everything
    // that lost its vector.
    //
    // The clear runs by opening lance in-process via a one-shot
    // StorageActor. CLAUDE.md invariant 3 requires single-writer-per-kb;
    // if the daemon is up the live actor is the writer and ours would
    // race with it. Refuse loudly in that case (deep-review C1). The
    // probe is best-effort — false negatives are still possible, so the
    // operator can pass `--in-place` after stopping the daemon.
    if in_place && old_dim == Some(info.dim) {
        if daemon_appears_running(&cfg.server.addr) {
            bail!(
                "daemon at {} appears to be running. Stop it before `kb model set --in-place` \
                 (CLAUDE.md invariant 3: only one writer per kb may hold the lance dataset; \
                 a concurrent in-place clear corrupts the daemon's open file handles).",
                cfg.server.addr
            );
        }
        eprintln!("in-place swap: priming lance for re-embed (call `kb daemon` to repopulate)");
        // Use the daemon name (matches every other path-resolution
        // site in the CLI) — pre-fix this passed the kb name as the
        // daemon name, which is always wrong (deep-review C2).
        let daemon_name = cfg.daemon.name.as_deref().unwrap_or("default");
        let paths = KbPaths::new(daemon_name)?;
        let lance = paths.kb_lance(&target_kb);
        let sqlite = paths.kb_sqlite(&target_kb);
        if lance.exists() {
            let handle = kb_core::storage::StorageActor::spawn(lance, sqlite, None).await?;
            handle.clear_embeddings().await?;
            handle.shutdown().await;
            println!("✓ embedding column cleared (run `kb daemon` to repopulate)");
        } else {
            eprintln!(
                "(no existing lance table at {}; nothing to clear)",
                paths.kb_state(&target_kb).display()
            );
        }
    }

    println!(
        "✓ kb {target_kb} embedding_model = {name:?} (in {})",
        cfg_path.display()
    );
    Ok(())
}

/// Same shape as `commands::reset::daemon_appears_running` — a TCP probe
/// against the configured listen address. False positives are possible
/// (another process holding the port), false negatives too on a busy
/// machine; this is a guardrail, not a guarantee. Stays local rather
/// than getting hoisted to a shared helper to keep the dependency
/// graph between commands flat.
///
/// K9: `to_socket_addrs` so `localhost:4000` (hostname) parses.
fn daemon_appears_running(addr: &str) -> bool {
    use std::net::ToSocketAddrs;
    let socket_addrs = match addr.to_socket_addrs() {
        Ok(it) => it.collect::<Vec<_>>(),
        Err(_) => return false,
    };
    socket_addrs
        .into_iter()
        .any(|sa| std::net::TcpStream::connect_timeout(&sa, Duration::from_millis(200)).is_ok())
}

fn remove_model(config_path: Option<&PathBuf>, name: &str, force: bool) -> Result<()> {
    let _info =
        model_info(name).ok_or_else(|| anyhow!("unknown model: {name:?}; try `kb model list`"))?;
    let cfg_path = resolve_config_path(config_path)?;
    let cfg = load_config_or_default(&cfg_path)?;

    let referencing: Vec<&KbName> = cfg
        .kb
        .iter()
        .filter(|(_, s)| s.embedding_model.as_deref() == Some(name))
        .map(|(k, _)| k)
        .collect();

    if !referencing.is_empty() && !force {
        let names: Vec<_> = referencing.iter().map(|k| k.as_str()).collect();
        return Err(anyhow!(
            "model {name:?} is referenced by kb(s): {}; \
             use `--force` to remove anyway (those kbs will lose semantic+hybrid search)",
            names.join(", ")
        ));
    }

    let cache = cache_dir()?;
    match cached_model_dir(&cache, name) {
        Some(model_dir) => {
            std::fs::remove_dir_all(&model_dir)?;
            println!("✓ removed {}", model_dir.display());
        }
        None => println!("(model {name:?} was not in cache; nothing to remove)"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_appears_running_false_for_unresolvable_addr() {
        // Reset.rs has the same shape; lock the contract for model.rs's
        // own copy. Post-K9 `to_socket_addrs` resolves hostnames, so
        // garbage strings still fail (DNS won't resolve them).
        assert!(!daemon_appears_running("not an addr"));
        assert!(!daemon_appears_running(""));
    }

    #[test]
    fn daemon_appears_running_false_for_unbound_port() {
        // Pick a port that's almost certainly not in use. Test isn't
        // perfect (port could be in use by other workspace tests) but
        // the failure mode is a noisy false-positive — not silent.
        assert!(!daemon_appears_running("127.0.0.1:1"));
    }

    #[test]
    fn daemon_appears_running_handles_hostname_port() {
        // K9 regression: `SocketAddr::parse` rejects `localhost:4000`
        // (hostname not IP) and the guard silently false-negatived.
        // Post-fix `to_socket_addrs` resolves `localhost` and the
        // probe runs. Port 1 should fail to connect; the contract is
        // that the FUNCTION RETURNED without panicking and gave a
        // false (no connection) rather than a parse-failure short-
        // circuit.
        assert!(!daemon_appears_running("localhost:1"));
    }

    #[test]
    fn daemon_default_line_uses_registry_fallback_when_unset() {
        let defaults = kb_core::config::DefaultsSection::default();
        let line = daemon_default_line(&defaults);
        assert!(
            line.contains("bge-small-en-v1.5") && line.contains("384"),
            "default rendering must name the registry default + dim: {line}"
        );
        assert!(line.contains("registry fallback"), "got: {line}");
    }

    #[test]
    fn daemon_default_line_uses_defaults_section_when_set() {
        let defaults = kb_core::config::DefaultsSection {
            embedding_model: Some("bge-large-en-v1.5".into()),
            ..Default::default()
        };
        let line = daemon_default_line(&defaults);
        assert!(
            line.contains("bge-large-en-v1.5") && line.contains("1024"),
            "rendering must name the [defaults] model + dim: {line}"
        );
        assert!(line.contains("from [defaults]"), "got: {line}");
    }

    #[test]
    fn daemon_default_line_flags_unknown_model_name() {
        let defaults = kb_core::config::DefaultsSection {
            embedding_model: Some("not-a-real-model".into()),
            ..Default::default()
        };
        let line = daemon_default_line(&defaults);
        assert!(line.contains("not-a-real-model"), "got: {line}");
        assert!(line.contains("unknown"), "got: {line}");
    }

    #[test]
    fn daemon_default_line_reports_disabled_when_fallback_off() {
        let defaults = kb_core::config::DefaultsSection {
            disable_embedder_fallback: true,
            ..Default::default()
        };
        let line = daemon_default_line(&defaults);
        assert!(line.contains("disable_embedder_fallback"), "got: {line}");
        assert!(line.contains("disabled"), "got: {line}");
    }

    #[test]
    fn in_place_uses_daemon_name_not_kb_name_for_paths() {
        // Regression test for deep-review C2: the in-place lance clear
        // used `KbPaths::new(target_kb.as_str())` (kb name as daemon
        // name) instead of `cfg.daemon.name`. With the fix, both kbs
        // configured against one daemon resolve to the same state
        // root, and `kb_lance(kb)` puts them in sibling subdirs.
        let paths = KbPaths::rooted_at(std::path::Path::new("/tmp/kb-test"), "myhost");
        let kb_a = KbName::new("notes").unwrap();
        let kb_b = KbName::new("research").unwrap();
        let lance_a = paths.kb_lance(&kb_a);
        let lance_b = paths.kb_lance(&kb_b);
        // Both kbs share the daemon-name parent dir; only the kb-name
        // suffix differs. Pre-C2 the kb name took the daemon-name slot
        // and the paths landed under unrelated roots.
        let parent_a = lance_a.parent().and_then(|p| p.parent()).unwrap();
        let parent_b = lance_b.parent().and_then(|p| p.parent()).unwrap();
        assert_eq!(
            parent_a, parent_b,
            "both kbs in the same daemon must share a state root"
        );
        assert!(
            parent_a.to_string_lossy().contains("myhost"),
            "state root should include the daemon name, got {parent_a:?}"
        );
    }
}
