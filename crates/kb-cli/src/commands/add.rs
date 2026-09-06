//! `kb add <path> [--kb name]` — register a source folder by writing it
//! into kb.toml. The daemon's watcher picks up the kb.toml change and arms
//! the new source automatically (per topic 02 §Decisions).

use super::{load_config_or_default, resolve_config_path};
use anyhow::{anyhow, Context, Result};
use kb_core::config::KbSection;
use kb_core::types::KbName;
use std::collections::btree_map::Entry;
use std::path::{Path, PathBuf};

pub fn run(
    config_path: Option<&PathBuf>,
    source_path: &Path,
    kb: &str,
    embedding_model: Option<&str>,
) -> Result<()> {
    let kb_name = KbName::new(kb).map_err(|e| anyhow!("invalid kb name {kb:?}: {e}"))?;

    // D3 — validate the model name against the registry up front so a
    // typo errors immediately instead of producing a kb.toml the
    // daemon would refuse at startup. The error lists registered
    // names so the operator can fix without `kb model list`.
    if let Some(name) = embedding_model {
        if kb_core::embed::model_info(name).is_none() {
            let registered: Vec<&str> = kb_core::embed::SUPPORTED_MODELS
                .iter()
                .map(|m| m.name)
                .collect();
            return Err(anyhow!(
                "unknown embedding model {name:?}; registered: {}",
                registered.join(", ")
            ));
        }
    }

    // Tilde expansion: shells normally expand `~` themselves, but
    // quoted arguments, `kb add "~/notes"`, or programmatic invocations
    // (cron, Claude Code task) bypass that. The pre-fix `canonicalize`
    // would then return a "not found" error for a path the user
    // reasonably typed. M-cli.
    let expanded = expand_tilde(source_path);
    let canonical = expanded
        .canonicalize()
        .with_context(|| format!("source path not found: {}", expanded.display()))?;
    if !canonical.is_dir() {
        return Err(anyhow!(
            "source path must be a directory: {}",
            canonical.display()
        ));
    }

    let cfg_path = resolve_config_path(config_path)?;
    let mut cfg = load_config_or_default(&cfg_path)?;

    // v0.7.1 H9 — if the kb is already registered, update ONLY its
    // source path (and embedding_model when the operator passed it).
    // A bare `insert` here used to replace the whole KbSection,
    // silently wiping embedding_model / outbound / atlas /
    // skip_patterns / templates — so re-running `kb add` to repoint a
    // configured kb dropped it to lexical-only search with no warning.
    let action = match cfg.kb.entry(kb_name.clone()) {
        Entry::Occupied(mut existing) => {
            existing.get_mut().path = canonical.clone();
            if let Some(name) = embedding_model {
                existing.get_mut().embedding_model = Some(name.to_string());
            }
            "Updated"
        }
        Entry::Vacant(slot) => {
            slot.insert(KbSection {
                path: canonical.clone(),
                skip_patterns: Vec::new(),
                ui: Default::default(),
                embedding_model: embedding_model.map(str::to_string),
                reranker_model: None,
                chunked_embeddings: false,
                graph_boost: None,
                outbound: None,
                atlas: None,
                templates: Default::default(),
                memory_scope: None,
                default_search_category: None,
                code_url: None,
                decay_policy: None,
                versions: None,
                reading_progress: None,
                search: Default::default(),
                indexable_extensions: None,
                reconcile_secs: None,
                capture_dir: None,
                resurface: None,
                slo: None,
            });
            "Registered"
        }
    };

    cfg.save(&cfg_path).with_context(|| {
        format!(
            "write {} (config dir may need to be created)",
            cfg_path.display()
        )
    })?;

    println!(
        "{action} kb {kb_name} → {}\nWrote {}",
        canonical.display(),
        cfg_path.display()
    );
    Ok(())
}

/// Expand a leading `~` to `$HOME` (or `~/` to `$HOME/`). Other tilde
/// forms (`~user/...`) are returned as-is — we don't link against
/// libnss-passwd here. Matches the common bash + zsh convention but
/// stays conservative.
fn expand_tilde(p: &Path) -> PathBuf {
    expand_tilde_with(p, std::env::var_os("HOME").map(PathBuf::from))
}

/// Pure variant of `expand_tilde` that takes the home dir explicitly.
/// LOW (deep-review): pre-fix the test mutated `$HOME` via
/// `std::env::set_var`, which Rust 1.84+ warns on for MT tests
/// (process-global env is racy when tests run in parallel). Now the
/// test calls the pure variant directly; production wires it via
/// `expand_tilde`.
fn expand_tilde_with(p: &Path, home: Option<PathBuf>) -> PathBuf {
    let Some(s) = p.to_str() else {
        return p.to_path_buf();
    };
    if s == "~" {
        if let Some(home) = home {
            return home;
        }
    } else if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = home {
            return home.join(rest);
        }
    }
    p.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kb_core::config::KbConfig;

    #[test]
    fn expand_tilde_resolves_bare_tilde_to_home() {
        let home = Some(PathBuf::from("/home/testuser"));
        assert_eq!(
            expand_tilde_with(Path::new("~"), home.clone()),
            PathBuf::from("/home/testuser")
        );
        assert_eq!(
            expand_tilde_with(Path::new("~/notes"), home.clone()),
            PathBuf::from("/home/testuser/notes")
        );
        assert_eq!(
            expand_tilde_with(Path::new("~/sub/dir"), home.clone()),
            PathBuf::from("/home/testuser/sub/dir")
        );
        // Embedded `~` is not expanded — only leading.
        assert_eq!(
            expand_tilde_with(Path::new("/abs/~/foo"), home.clone()),
            PathBuf::from("/abs/~/foo")
        );
        // Other-user tilde (`~bob/...`) is not expanded.
        assert_eq!(
            expand_tilde_with(Path::new("~bob/x"), home.clone()),
            PathBuf::from("~bob/x")
        );
        // HOME unset: leading `~` left as-is.
        assert_eq!(expand_tilde_with(Path::new("~"), None), PathBuf::from("~"));
        assert_eq!(
            expand_tilde_with(Path::new("~/foo"), None),
            PathBuf::from("~/foo")
        );
    }

    #[test]
    fn re_adding_a_kb_preserves_config_and_updates_path() {
        // v0.7.1 H9 — `kb add` on an already-registered kb updates only
        // the source path; it must not wipe a hand-configured
        // embedding_model / skip_patterns / outbound / atlas.
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("kb.toml");
        let src_a = tmp.path().join("src-a");
        let src_b = tmp.path().join("src-b");
        std::fs::create_dir_all(&src_a).unwrap();
        std::fs::create_dir_all(&src_b).unwrap();

        // First add → registers a fresh section.
        run(Some(&cfg_path), &src_a, "work", None).unwrap();

        // Operator hand-configures the embedder + a skip pattern.
        let mut cfg = KbConfig::load(&cfg_path).unwrap();
        let kb_name = KbName::new("work").unwrap();
        {
            let section = cfg.kb.get_mut(&kb_name).unwrap();
            section.embedding_model = Some("bge-small-en-v1.5".to_string());
            section.skip_patterns = vec!["*.tmp".to_string()];
        }
        cfg.save(&cfg_path).unwrap();

        // Re-add the same kb pointing at a different folder (no model
        // override → keep the hand-configured value).
        run(Some(&cfg_path), &src_b, "work", None).unwrap();

        let cfg = KbConfig::load(&cfg_path).unwrap();
        let section = cfg.kb.get(&kb_name).unwrap();
        assert_eq!(
            section.path,
            src_b.canonicalize().unwrap(),
            "the source path should be updated"
        );
        assert_eq!(
            section.embedding_model.as_deref(),
            Some("bge-small-en-v1.5"),
            "re-add must not wipe embedding_model"
        );
        assert_eq!(
            section.skip_patterns,
            vec!["*.tmp".to_string()],
            "re-add must not wipe skip_patterns"
        );
    }

    #[test]
    fn add_with_embedding_model_writes_field() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("kb.toml");
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();

        run(Some(&cfg_path), &src, "work", Some("bge-large-en-v1.5")).unwrap();

        let cfg = KbConfig::load(&cfg_path).unwrap();
        let section = cfg.kb.get(&KbName::new("work").unwrap()).unwrap();
        assert_eq!(
            section.embedding_model.as_deref(),
            Some("bge-large-en-v1.5"),
            "--embedding-model must write to the section's embedding_model field"
        );
    }

    #[test]
    fn add_with_unknown_embedding_model_errors_and_lists_registered() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("kb.toml");
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();

        let err = run(Some(&cfg_path), &src, "work", Some("not-a-real-model"))
            .expect_err("unknown model must surface as an error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("not-a-real-model"),
            "error must echo the bad name; got: {msg}"
        );
        // Lists at least one registered name so the operator can fix.
        assert!(
            msg.contains("bge-small-en-v1.5"),
            "error must list registered names; got: {msg}"
        );
        assert!(
            !cfg_path.exists(),
            "kb.toml must NOT be written when the model name is rejected"
        );
    }

    #[test]
    fn re_add_with_embedding_model_updates_existing_section() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("kb.toml");
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();

        // First add — no model.
        run(Some(&cfg_path), &src, "work", None).unwrap();
        let cfg = KbConfig::load(&cfg_path).unwrap();
        assert!(cfg
            .kb
            .get(&KbName::new("work").unwrap())
            .unwrap()
            .embedding_model
            .is_none());

        // Re-add WITH a model — the existing section gains the field.
        run(Some(&cfg_path), &src, "work", Some("bge-base-en-v1.5")).unwrap();
        let cfg = KbConfig::load(&cfg_path).unwrap();
        assert_eq!(
            cfg.kb
                .get(&KbName::new("work").unwrap())
                .unwrap()
                .embedding_model
                .as_deref(),
            Some("bge-base-en-v1.5"),
            "re-add with --embedding-model updates the existing section"
        );
    }
}
