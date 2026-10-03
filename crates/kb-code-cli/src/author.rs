//! v0.44 F5 — ONE author rule for every row the kb-code CLI creates.
//!
//! Before this, `annotate`/`annotate reply` sent no `author`, so the daemon
//! stamped the human `you` on every agent reply — the Room's awaiting-agent
//! chip never flipped, the "agent replied" toast never fired, the inbox kept
//! counting the thread, and `annotate watch --ignore-author claude` showed
//! the agent its own replies back. The rule mirrors the `kb slate` CLI:
//!
//! `--author X` > `--as you` > `$KB_CODE_AUTHOR` > `$KB_HARNESS` > `claude`
//!
//! `claude` is a flagged last guess: a HUMAN scripting the CLI without
//! `--as you` is saved as an agent. That is the documented trade (an
//! unattributed human write is rarer than an agent write through this CLI);
//! `--as you` is the opt-out. Attribution only — one trust tier, no auth.

use anyhow::{bail, Result};

/// The agent author names the daemon recognises
/// (`kb_code_server::review_timeline::AGENT_AUTHOR_NAMES`).
pub fn agent_names() -> &'static [&'static str] {
    kb_code_server::review_timeline::AGENT_AUTHOR_NAMES
}

/// Pure ladder over an injected env lookup.
pub fn resolve_author(
    author: Option<&str>,
    as_who: Option<&str>,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<String> {
    let pick = |s: Option<&str>| {
        let t = s?.trim();
        (!t.is_empty()).then(|| t.to_string())
    };
    if let Some(a) = pick(author) {
        return Ok(a);
    }
    if let Some(w) = pick(as_who) {
        return match w.as_str() {
            "you" => Ok("you".to_string()),
            other => bail!(
                "--as must be `you` (a human at the terminal), got {other:?} — name an agent with --author"
            ),
        };
    }
    for var in ["KB_CODE_AUTHOR", "KB_HARNESS"] {
        if let Some(v) = pick(env(var).as_deref()) {
            return Ok(v);
        }
    }
    Ok("claude".to_string())
}

/// Production entry over the process environment.
pub fn author_for_write(author: Option<&str>, as_who: Option<&str>) -> Result<String> {
    resolve_author(author, as_who, &|k| std::env::var(k).ok())
}

/// `--ignore-agents` expands to every agent author name, merged with any
/// explicit `--ignore-author` values (no duplicates).
pub fn expand_ignore(mut ignore_author: Vec<String>, ignore_agents: bool) -> Vec<String> {
    if ignore_agents {
        for n in agent_names() {
            if !ignore_author.iter().any(|i| i == n) {
                ignore_author.push((*n).to_string());
            }
        }
    }
    ignore_author
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k: &str| m.get(k).cloned()
    }

    #[test]
    fn ladder_flag_then_as_you_then_code_author_then_harness_then_claude() {
        let e = env(&[("KB_CODE_AUTHOR", "codex"), ("KB_HARNESS", "omp")]);
        assert_eq!(
            resolve_author(Some("grok"), Some("you"), &e).unwrap(),
            "grok"
        );
        assert_eq!(resolve_author(None, Some("you"), &e).unwrap(), "you");
        assert_eq!(resolve_author(None, None, &e).unwrap(), "codex");
        let e = env(&[("KB_HARNESS", "omp")]);
        assert_eq!(resolve_author(None, None, &e).unwrap(), "omp");
        let e = env(&[]);
        assert_eq!(resolve_author(None, None, &e).unwrap(), "claude");
        // blanks fall through
        let e = env(&[("KB_CODE_AUTHOR", "  "), ("KB_HARNESS", "")]);
        assert_eq!(resolve_author(Some(" "), None, &e).unwrap(), "claude");
    }

    #[test]
    fn as_accepts_only_you() {
        assert!(resolve_author(None, Some("omp"), &env(&[])).is_err());
    }

    #[test]
    fn ignore_agents_covers_every_server_agent_name_once() {
        let v = expand_ignore(vec!["claude".to_string(), "someone".to_string()], true);
        for n in agent_names() {
            assert_eq!(v.iter().filter(|x| x == n).count(), 1, "{n}");
        }
        assert!(v.contains(&"someone".to_string()));
        assert_eq!(
            expand_ignore(vec!["x".into()], false),
            vec!["x".to_string()]
        );
    }
}
