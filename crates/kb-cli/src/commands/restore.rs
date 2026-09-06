//! `kb restore <tarball> --kb <name> [--force]` — extract a `kb backup`
//! tarball back into a kb's state directory. The destructive half of
//! backup: it refuses to overwrite an existing non-empty state unless
//! `--force` (which wipes it first), and the daemon for that kb must be
//! stopped first — it holds `index.db` open.
//!
//! SL2 — a tarball also carries the daemon-wide `slates/` store beside
//! `<kb>/`. It is restored ONLY when the destination has none: `--force`
//! means "replace this kb", and a slate is not per-kb.

use super::{load_config_or_default, resolve_config_path};
use anyhow::{anyhow, Context, Result};
use kb_core::paths::KbPaths;
use kb_core::types::KbName;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn run(config_path: Option<&PathBuf>, kb: &str, tarball: &Path, force: bool) -> Result<()> {
    let kb_name = KbName::new(kb).map_err(|e| anyhow!("invalid kb {kb:?}: {e}"))?;
    if !tarball.exists() {
        return Err(anyhow!("tarball {} not found", tarball.display()));
    }
    let cfg_path = resolve_config_path(config_path)?;
    let cfg = load_config_or_default(&cfg_path)?;
    let paths = KbPaths::new(cfg.daemon.name.as_deref().unwrap_or("default"))?;
    paths.ensure_dirs()?;

    let kb_state = paths.kb_state(&kb_name);
    if kb_state.exists() {
        let non_empty = std::fs::read_dir(&kb_state)?.next().is_some();
        if non_empty && !force {
            return Err(anyhow!(
                "kb {kb_name} already has state at {} — refusing to overwrite. \
                 Stop the daemon for this kb, then pass --force to replace it.",
                kb_state.display()
            ));
        }
        if force {
            std::fs::remove_dir_all(&kb_state)
                .with_context(|| format!("clearing existing state {}", kb_state.display()))?;
        }
    }

    // The tarball root is `<kb>/…`; extracting into the kb-state PARENT
    // recreates `<state>/<daemon>/<kb>/`.
    let parent = kb_state
        .parent()
        .ok_or_else(|| anyhow!("kb state has no parent: {}", kb_state.display()))?;
    std::fs::create_dir_all(parent)?;

    // The kb's own tree, always. Named explicitly (rather than extracting
    // the whole archive) so the daemon-wide `slates/` member below is a
    // SEPARATE, guarded decision — see its comment.
    let status = Command::new("tar")
        .arg("-xzf")
        .arg(tarball)
        .arg("-C")
        .arg(parent)
        .arg(kb_name.as_str())
        .status()
        .with_context(|| "tar invocation failed (is `tar` installed?)")?;
    if !status.success() {
        return Err(anyhow!("tar returned exit code {status}"));
    }

    // SL2 — the daemon-wide `slates/` clause (design §7 "Registration").
    // A backup tarball carries `<kb>/` AND, since SL2, a sibling
    // `slates/`; `parent` here IS `<state>`, so extracting it lands the
    // slates back where `KbPaths::slate_dir` looks for them.
    //
    // NEVER clobbers. `--force` is scoped to "replace THIS kb's state" —
    // slates are daemon-wide, so honouring it here would blow away every
    // other project's live coordination board on a single-kb restore.
    // An existing `slates/` is therefore left alone and the skip is
    // printed, with the remedy (move it aside and re-run) named. Purge
    // stays the sole deletion path for a slate.
    let slates_dir = parent.join("slates");
    if archive_has_slates(tarball)? {
        if slates_dir.exists() {
            println!(
                "restore: SKIPPING the tarball's daemon-wide slates/ — {} already exists.                  Slates are not per-kb, so --force (which replaces only kb {kb_name}) does not                  cover them; move that directory aside and re-run to restore them.",
                slates_dir.display()
            );
        } else {
            let status = Command::new("tar")
                .arg("-xzf")
                .arg(tarball)
                .arg("-C")
                .arg(parent)
                .arg("slates")
                .status()
                .with_context(|| "tar invocation failed (is `tar` installed?)")?;
            if !status.success() {
                return Err(anyhow!(
                    "tar returned exit code {status} extracting slates/"
                ));
            }
            println!("restore: slates/ → {}", slates_dir.display());
        }
    }

    // Validate: the archive must have produced this kb's index.db. Catches
    // a wrong tarball, or a --kb that doesn't match the archive's root dir.
    if !kb_state.join("index.db").exists() {
        return Err(anyhow!(
            "restore produced no index.db at {} — wrong tarball, or its root dir \
             doesn't match --kb {kb_name}?",
            kb_state.display()
        ));
    }

    println!("restore: {} → {}", tarball.display(), kb_state.display());
    Ok(())
}

/// Does this tarball carry the SL2 daemon-wide `slates/` member? Pre-SL2
/// tarballs do not, and asking `tar` to extract a missing member is an
/// error — so the listing is the gate rather than a swallowed failure.
fn archive_has_slates(tarball: &Path) -> Result<bool> {
    let out = Command::new("tar")
        .arg("-tzf")
        .arg(tarball)
        .output()
        .with_context(|| "tar invocation failed (is `tar` installed?)")?;
    if !out.status.success() {
        return Err(anyhow!(
            "tar -tzf {} failed: {}",
            tarball.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .any(|l| l == "slates" || l == "slates/" || l.starts_with("slates/")))
}
