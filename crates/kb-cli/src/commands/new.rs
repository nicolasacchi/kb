//! `kb new` — scaffold a new artifact from a template HTML file.
//!
//! Usage:
//!
//!   kb new --template <path-or-name> --title "..." [--out <path>] \
//!          [--var key=value ...] [--kb <name>]
//!
//! Substitutes `{{title}}`, `{{date}}`, `{{slug}}`, and any user-supplied
//! `--var key=value` placeholders in the template, then writes to `--out`
//! (or stdout if absent).
//!
//! Template resolution:
//!   - If `--template` looks like a path (contains `/` or `.`) — used as-is.
//!   - Otherwise looked up as a short name in the kb's
//!     `[kb.<name>.templates]` table in kb.toml. The kb is the one passed
//!     via `--kb`, or the sole kb in the config, or "default".
//!
//! Missing placeholders are left as-is in the output (intentional — a
//! later hand-edit can fill them). No error, just a stderr note listing
//! the keys that survived substitution.

use super::{load_config_or_default, resolve_config_path};
use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Entrypoint. Wired from `main.rs`.
pub fn run(
    config_path: Option<&PathBuf>,
    template: &str,
    title: &str,
    out: Option<&Path>,
    vars: &[(String, String)],
    kb: Option<&str>,
) -> Result<()> {
    let template_path = resolve_template_path(config_path, template, kb)?;
    let raw = std::fs::read_to_string(&template_path)
        .with_context(|| format!("read template {}", template_path.display()))?;

    let subs = build_subs(title, vars);
    let rendered = render(&raw, &subs);

    let remaining = remaining_placeholders(&rendered);
    if !remaining.is_empty() {
        eprintln!(
            "kb new: {} placeholder(s) survived substitution: {}",
            remaining.len(),
            remaining.join(", ")
        );
    }

    match out {
        Some(p) => {
            if let Some(parent) = p.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent)
                        .with_context(|| format!("mkdir -p {}", parent.display()))?;
                }
            }
            std::fs::write(p, &rendered).with_context(|| format!("write {}", p.display()))?;
            eprintln!("kb new: wrote {}", p.display());
        }
        None => {
            print!("{rendered}");
        }
    }
    Ok(())
}

/// Resolve `--template` to a real path on disk.
fn resolve_template_path(
    config_path: Option<&PathBuf>,
    template: &str,
    kb_arg: Option<&str>,
) -> Result<PathBuf> {
    // If it looks like a path, use it directly.
    if looks_like_path(template) {
        let p = PathBuf::from(template);
        if !p.exists() {
            bail!("template path does not exist: {}", p.display());
        }
        return Ok(p);
    }

    // Otherwise — name lookup against kb.toml `[kb.<name>.templates]`.
    let cfg_path = resolve_config_path(config_path)?;
    let cfg = load_config_or_default(&cfg_path)?;

    let kb_name = pick_kb(&cfg, kb_arg)?;
    let Some(section) = cfg.kb.get(&kb_name) else {
        bail!(
            "kb '{kb_name}' not configured in {} — pass --template as a \
             path instead, or add a [kb.{kb_name}] section",
            cfg_path.display()
        );
    };
    let Some(path) = section.templates.get(template) else {
        bail!(
            "no template named '{template}' in [kb.{kb_name}.templates]; \
             configured names: {:?}",
            section.templates.keys().collect::<Vec<_>>()
        );
    };
    if !path.exists() {
        bail!(
            "template '{template}' → {} (configured) does not exist",
            path.display()
        );
    }
    Ok(path.clone())
}

fn pick_kb(
    cfg: &kb_core::config::KbConfig,
    kb_arg: Option<&str>,
) -> Result<kb_core::types::KbName> {
    use kb_core::types::KbName;
    if let Some(name) = kb_arg {
        return KbName::new(name).map_err(|e| anyhow::anyhow!("{e}"));
    }
    if cfg.kb.len() == 1 {
        return Ok(cfg.kb.keys().next().unwrap().clone());
    }
    if cfg.kb.is_empty() {
        bail!("no kbs configured — pass --kb or --template as a path");
    }
    bail!(
        "multiple kbs configured ({:?}); specify --kb",
        cfg.kb.keys().collect::<Vec<_>>()
    );
}

fn looks_like_path(s: &str) -> bool {
    s.contains('/') || s.contains('\\') || s.starts_with('.') || s.ends_with(".html")
}

fn build_subs(title: &str, vars: &[(String, String)]) -> HashMap<String, String> {
    let mut subs = HashMap::new();
    subs.insert("title".to_string(), title.to_string());
    subs.insert("date".to_string(), today_iso());
    subs.insert("slug".to_string(), slugify(title));
    for (k, v) in vars {
        subs.insert(k.clone(), v.clone());
    }
    subs
}

fn today_iso() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in s.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// `{{key}}` → value. Keys not in `subs` survive verbatim (no error).
fn render(raw: &str, subs: &HashMap<String, String>) -> String {
    let mut out = String::with_capacity(raw.len());
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Find next `{{` at position i. `{` is ASCII so byte-level
        // checks here are safe — `i` is always at a char boundary
        // because the non-match arm below advances by full UTF-8 chars.
        if i + 1 < bytes.len() && bytes[i] == b'{' && bytes[i + 1] == b'{' {
            // Find matching `}}`.
            if let Some(end_rel) = find_double_brace_close(&raw[i + 2..]) {
                let key = raw[i + 2..i + 2 + end_rel].trim().to_string();
                if let Some(value) = subs.get(&key) {
                    out.push_str(value);
                } else {
                    out.push_str(&raw[i..i + 2 + end_rel + 2]);
                }
                i += 2 + end_rel + 2;
                continue;
            }
        }
        // No template at i. Advance by ONE UTF-8 CHAR (1-4 bytes),
        // not one byte. `raw.as_bytes()[i] as char` (the pre-fix
        // version, deep-review C3) interpreted each byte as its own
        // codepoint, mangling every non-ASCII character — CJK, accented
        // latin, emoji, smart quotes — into mojibake.
        let ch = raw[i..]
            .chars()
            .next()
            .expect("i < bytes.len() implies at least one char remains");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Find the byte offset of `}}` in `rest`, or None.
fn find_double_brace_close(rest: &str) -> Option<usize> {
    let bytes = rest.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'}' && bytes[i + 1] == b'}' {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Surfaces unfilled `{{placeholder}}` keys remaining in the rendered
/// output so the user knows what to hand-fill. Deduplicates + sorts.
fn remaining_placeholders(rendered: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    let bytes = rendered.as_bytes();
    while i + 1 < bytes.len() {
        if bytes[i] == b'{' && bytes[i + 1] == b'{' {
            if let Some(end_rel) = find_double_brace_close(&rendered[i + 2..]) {
                let key = rendered[i + 2..i + 2 + end_rel].trim().to_string();
                if !out.contains(&key) {
                    out.push(key);
                }
                i += 2 + end_rel + 2;
                continue;
            }
        }
        i += 1;
    }
    out.sort();
    out
}

/// Parses `key=value` strings from `--var`. Errors on missing `=`.
pub fn parse_var(spec: &str) -> Result<(String, String)> {
    match spec.split_once('=') {
        Some((k, v)) => {
            let k = k.trim().to_string();
            if k.is_empty() {
                bail!("--var key must be non-empty: {spec:?}");
            }
            Ok((k, v.to_string()))
        }
        None => bail!("--var must be key=value, got {spec:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subs(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn renders_simple_substitution() {
        let s = render("hello {{name}}!", &subs(&[("name", "world")]));
        assert_eq!(s, "hello world!");
    }

    #[test]
    fn preserves_unknown_placeholders() {
        let s = render("{{a}} and {{b}}", &subs(&[("a", "X")]));
        assert_eq!(s, "X and {{b}}");
    }

    #[test]
    fn preserves_single_brace() {
        let s = render("a { b } c", &subs(&[]));
        assert_eq!(s, "a { b } c");
    }

    #[test]
    fn handles_back_to_back_placeholders() {
        let s = render("{{a}}{{b}}", &subs(&[("a", "1"), ("b", "2")]));
        assert_eq!(s, "12");
    }

    #[test]
    fn render_strips_surrounding_whitespace_in_key() {
        let s = render("{{  title  }}", &subs(&[("title", "T")]));
        assert_eq!(s, "T");
    }

    #[test]
    fn render_preserves_non_ascii_chars_verbatim() {
        // C3 regression: pre-fix `render` walked the input byte-by-byte
        // and did `as_bytes()[i] as char`, splitting every multi-byte
        // UTF-8 sequence into individual latin-1 codepoints. A template
        // with CJK / accented latin / emoji / smart quotes became
        // mojibake. The fix advances by `ch.len_utf8()` instead.
        let s = render(
            "héllo 世界 — “smart quotes” 🦀 {{name}}!",
            &subs(&[("name", "wörld")]),
        );
        assert_eq!(s, "héllo 世界 — “smart quotes” 🦀 wörld!");
    }

    #[test]
    fn render_preserves_non_ascii_with_no_substitutions() {
        // No `{{...}}` at all — pure passthrough path. Confirms the
        // non-template arm is the one being fixed (not a side effect
        // of the substitution path).
        let s = render("ünterveränderlich 漢字 🎉", &subs(&[]));
        assert_eq!(s, "ünterveränderlich 漢字 🎉");
    }

    #[test]
    fn slugify_strips_punct_and_lowercases() {
        assert_eq!(slugify("Hello, World!"), "hello-world");
        assert_eq!(slugify("  Lots   of   space  "), "lots-of-space");
        assert_eq!(slugify("---"), "");
    }

    #[test]
    fn parse_var_accepts_key_eq_value() {
        let (k, v) = parse_var("status=open").unwrap();
        assert_eq!(k, "status");
        assert_eq!(v, "open");
    }

    #[test]
    fn parse_var_empty_value_ok() {
        let (k, v) = parse_var("note=").unwrap();
        assert_eq!(k, "note");
        assert_eq!(v, "");
    }

    #[test]
    fn parse_var_rejects_missing_eq() {
        assert!(parse_var("status").is_err());
    }

    #[test]
    fn parse_var_rejects_empty_key() {
        assert!(parse_var("=value").is_err());
    }

    #[test]
    fn build_subs_includes_date_and_slug() {
        let s = build_subs("Hello World", &[]);
        assert_eq!(s.get("title").unwrap(), "Hello World");
        assert_eq!(s.get("slug").unwrap(), "hello-world");
        let d = s.get("date").unwrap();
        assert_eq!(d.len(), 10); // YYYY-MM-DD
        assert!(d.chars().nth(4) == Some('-'));
    }

    #[test]
    fn user_vars_override_built_in_keys() {
        let s = build_subs("T", &[("date".to_string(), "1999-12-31".to_string())]);
        assert_eq!(s.get("date").unwrap(), "1999-12-31");
    }

    #[test]
    fn remaining_placeholders_lists_unfilled_keys_sorted_and_deduped() {
        let r = remaining_placeholders("{{zeta}} {{alpha}} {{alpha}} text");
        assert_eq!(r, vec!["alpha", "zeta"]);
    }

    #[test]
    fn remaining_placeholders_empty_when_all_filled() {
        let r = remaining_placeholders("hello world");
        assert!(r.is_empty());
    }

    #[test]
    fn looks_like_path_detection() {
        assert!(looks_like_path("./foo.html"));
        assert!(looks_like_path("/abs/path.html"));
        assert!(looks_like_path("templates/idea.html"));
        assert!(looks_like_path("idea.html"));
        assert!(!looks_like_path("idea"));
        assert!(!looks_like_path("fix"));
    }
}
