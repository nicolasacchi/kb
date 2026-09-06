//! `kb config {show,validate,edit}` — inspect / validate / edit the
//! resolved kb.toml directly on disk. File-direct (no running daemon
//! needed), so it's a CI / pre-commit gate as well as an operator tool.
//! Complements the web Settings → Config tab, which does LIVE edits +
//! an in-process daemon restart; a `kb config edit` here applies only
//! after a daemon restart (it prints the reminder).

use super::{load_config_or_default, resolve_config_path};
use anyhow::{Context, Result};
use kb_core::config::KbConfig;
use std::path::PathBuf;

/// `kb config show` — print the resolved path + the parsed config as TOML.
pub fn show(config_path: Option<&PathBuf>) -> Result<()> {
    let path = resolve_config_path(config_path)?;
    let cfg = load_config_or_default(&path)?;
    println!("# {}", path.display());
    print!("{}", cfg.to_string_pretty()?);
    Ok(())
}

/// Print every validation issue to stderr; return the hard-error count.
fn report_issues(cfg: &KbConfig) -> usize {
    let issues = cfg.validate();
    for i in &issues {
        let glyph = if i.is_hard() {
            "✗ error"
        } else {
            "⚠ warn "
        };
        eprintln!("{glyph}  {}  —  {}", i.pointer, i.message);
    }
    issues.iter().filter(|i| i.is_hard()).count()
}

/// `kb config validate` — validate the resolved config; exit nonzero when
/// any issue is hard (the contract a CI / pre-commit hook checks).
pub fn validate(config_path: Option<&PathBuf>) -> Result<()> {
    let path = resolve_config_path(config_path)?;
    let cfg = load_config_or_default(&path)?;
    let hard = report_issues(&cfg);
    if hard > 0 {
        anyhow::bail!("{hard} hard config error(s) in {}", path.display());
    }
    eprintln!("✓ {} — no errors", path.display());
    Ok(())
}

/// `kb config edit` — open `$VISUAL`/`$EDITOR` (else `vi`) on the resolved
/// file, then validate the result. Does NOT restart a running daemon;
/// prints how to apply.
pub fn edit(config_path: Option<&PathBuf>) -> Result<()> {
    let path = resolve_config_path(config_path)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_string());
    let status = std::process::Command::new(&editor)
        .arg(&path)
        .status()
        .with_context(|| format!("launch editor `{editor}`"))?;
    if !status.success() {
        anyhow::bail!("editor `{editor}` exited with {status}");
    }
    if !path.exists() {
        eprintln!("no file written");
        return Ok(());
    }
    // Re-parse + validate what was saved.
    let cfg = KbConfig::load(&path).with_context(|| format!("re-parse {}", path.display()))?;
    let hard = report_issues(&cfg);
    if hard > 0 {
        anyhow::bail!("{hard} hard error(s) — re-edit to fix");
    }
    eprintln!(
        "saved {}. Restart the daemon (or use Settings → Config in the web UI) to apply.",
        path.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_ok_on_good_config() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("kb.toml");
        std::fs::write(&p, "[server]\naddr = \"127.0.0.1:4000\"\n").unwrap();
        assert!(validate(Some(&p)).is_ok());
        assert!(show(Some(&p)).is_ok());
    }

    #[test]
    fn validate_errs_on_hard_issue() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("kb.toml");
        std::fs::write(&p, "[server]\naddr = \"not-an-addr\"\n").unwrap();
        assert!(validate(Some(&p)).is_err(), "a bad addr must exit non-zero");
    }
}
