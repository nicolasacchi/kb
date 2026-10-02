//! `kb restore <tarball> --kb <name> [--force]` — extract a `kb backup`
//! tarball back into a kb's state directory. The destructive half of
//! backup: it refuses to overwrite an existing non-empty state unless
//! `--force` (which wipes it first), and the daemon for that kb must be
//! stopped first — it holds `index.db` open.
//!
//! SL2 — a tarball may also carry the daemon-scope members of
//! `KbPaths::state_members` beside `<kb>/`: the `slates/` store and the
//! `saved-queries.json` / `memory-policy.json` / `tombstone-era.json`
//! files. Each is restored ONLY when the destination lacks it: `--force`
//! means "replace this kb", and these are not per-kb.
//!
//! The per-kb members (`index.db`, `lance/`, `.review/`, `.attachments/`,
//! `.proposals/`) all live under `<kb>/`, so extracting that one root
//! restores every one of them; `--force` clears the whole kb state first,
//! which is why a tarball that PREDATES `.attachments/`/`.proposals/` in
//! the backup scope warns before it deletes the live ones.

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
    let listing = archive_listing(tarball)?;
    if force {
        for dir in predated_members(&listing, kb_name.as_str(), &kb_state) {
            eprintln!(
                "restore: WARNING --force will DELETE {} — this tarball predates it in the \
                 backup scope and carries no {dir}/ to put back",
                kb_state.join(dir).display()
            );
        }
    }
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

    // SL2 — the daemon-scope members. A backup tarball carries `<kb>/`
    // AND, for the kb that was packed with them, sibling `slates/` and
    // daemon JSON files; `parent` here IS `<state>`, so extracting them
    // lands each where `KbPaths` looks for it.
    //
    // NEVER clobbers. `--force` is scoped to "replace THIS kb's state" —
    // these are daemon-wide, so honouring it here would blow away every
    // other project's live coordination board (or the live saved queries
    // and decay policy) on a single-kb restore. An existing member is
    // therefore left alone and the skip is printed, with the remedy (move
    // it aside and re-run) named. Purge stays the sole deletion path for a
    // slate.
    for member in daemon_members_in(&listing) {
        let dest = parent.join(member);
        if dest.exists() {
            println!(
                "restore: SKIPPING the tarball's daemon-wide {member} — {} already exists. \
                 It is not per-kb, so --force (which replaces only kb {kb_name}) does not \
                 cover it; move it aside and re-run to restore it.",
                dest.display()
            );
            continue;
        }
        let status = Command::new("tar")
            .arg("-xzf")
            .arg(tarball)
            .arg("-C")
            .arg(parent)
            .arg(member)
            .status()
            .with_context(|| "tar invocation failed (is `tar` installed?)")?;
        if !status.success() {
            return Err(anyhow!(
                "tar returned exit code {status} extracting {member}"
            ));
        }
        println!("restore: {member} → {}", dest.display());
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

/// Daemon-scope members a tarball can carry at its root, from
/// `KbPaths::state_members` (`StateScope::Daemon`, persistent).
const DAEMON_MEMBERS: [&str; 4] = [
    "slates",
    "saved-queries.json",
    "memory-policy.json",
    "tombstone-era.json",
];

/// The tarball's member paths, one per entry. Pre-SL2 tarballs carry no
/// daemon members, and asking `tar` to extract a missing member is an
/// error — so the listing is the gate rather than a swallowed failure.
fn archive_listing(tarball: &Path) -> Result<Vec<String>> {
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
        .map(|l| l.trim_end_matches('/').to_string())
        .collect())
}

/// Which of [`DAEMON_MEMBERS`] the listing carries, in registry order.
fn daemon_members_in(listing: &[String]) -> Vec<&'static str> {
    DAEMON_MEMBERS
        .into_iter()
        .filter(|member| {
            let member: &str = member;
            listing.iter().any(|l| {
                l.as_str() == member
                    || l.strip_prefix(member)
                        .is_some_and(|rest| rest.starts_with('/'))
            })
        })
        .collect()
}

/// Per-kb sidecar dirs that exist live under `kb_state` but are absent
/// from the archive: a `--force` restore would delete them.
fn predated_members(listing: &[String], kb: &str, kb_state: &Path) -> Vec<&'static str> {
    [".attachments", ".proposals"]
        .into_iter()
        .filter(|dir| kb_state.join(dir).exists())
        .filter(|dir| {
            let root = format!("{kb}/{dir}");
            !listing
                .iter()
                .any(|l| *l == root || l.starts_with(&format!("{root}/")))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// v0.44 B1 — daemon JSON files ride the tarball root beside `slates/`
    /// and must be recognised; a kb dir named like one must not be.
    #[test]
    fn daemon_members_are_recognised_at_the_tarball_root_only() {
        let listing = l(&[
            "notes",
            "notes/index.db",
            "notes/slates",
            "slates",
            "slates/proj/ledger.jsonl",
            "saved-queries.json",
            "tombstone-era.json",
        ]);
        assert_eq!(
            daemon_members_in(&listing),
            vec!["slates", "saved-queries.json", "tombstone-era.json"]
        );
        assert!(daemon_members_in(&l(&["notes", "notes/slates/x"])).is_empty());
    }

    /// A pre-v0.44 tarball has no `.attachments/`/`.proposals/`; `--force`
    /// would delete the live ones, so the restore must say so first.
    #[test]
    fn force_warns_when_the_archive_predates_a_live_sidecar_dir() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".attachments/a1")).unwrap();
        std::fs::create_dir_all(tmp.path().join(".proposals")).unwrap();
        let old = l(&["notes", "notes/index.db", "notes/.review/c.json"]);
        assert_eq!(
            predated_members(&old, "notes", tmp.path()),
            vec![".attachments", ".proposals"]
        );
        let new = l(&[
            "notes/index.db",
            "notes/.attachments",
            "notes/.attachments/a1/blob",
            "notes/.proposals/p.json",
        ]);
        assert!(predated_members(&new, "notes", tmp.path()).is_empty());
        // Nothing live to lose → nothing to warn about.
        let empty = tempfile::tempdir().unwrap();
        assert!(predated_members(&old, "notes", empty.path()).is_empty());
    }
}
