//! One session + harness identity ladder for every CLI write (v0.44 F5).
//!
//! `kb remember`, `kb notes new`, `kb desk offer` and every `kb slate` verb
//! used to decide "which session is this?" three different ways, and two of
//! them ended at ONE global, last-writer-wins `~/.cache/kb/current-session`
//! that every concurrent session's hooks overwrite. This module is the one
//! place that answers it.
//!
//! Ladder (first non-blank wins):
//!
//! 1. `--session-id` flag
//! 2. `KB_SESSION_ID`
//! 3. `CLAUDE_CODE_SESSION_ID` (harness `claude`)
//! 4. `GROK_SESSION_ID` (harness `grok`)
//! 5. the repo-keyed marker `current-session-repo-<slug>`, only while fresh
//!    (the same 40-minute rule `trailer-logic.sh` applies)
//! 6. the legacy global `current-session` file — a FLAGGED fallback
//!    ([`Source::LegacyFile`]); callers surface a stderr note because it is
//!    last-writer-wins across concurrent sessions
//! 7. none — slate lets the daemon stamp `unattributed`; remember/notes
//!    write no `kb-session`
//!
//! The resolver is pure: it takes an injected env lookup and the marker
//! contents, so the whole ladder is unit-tested without touching process
//! state. Attribution only — no authentication (one trust tier).

use std::path::Path;

/// Which rung supplied the session id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Flag,
    KbSessionId,
    ClaudeEnv,
    GrokEnv,
    RepoMarker,
    /// The global last-writer-wins file. Flagged: other sessions may have
    /// overwritten it.
    LegacyFile,
    None,
}

/// Where the harness name came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarnessSource {
    Flag,
    KbHarness,
    SessionEnv,
    /// Last-resort guess (`claude`) — flagged, never verified.
    Guess,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub session_id: Option<String>,
    pub source: Source,
    pub harness: String,
    /// Read by the ladder tests; callers that want to flag a guessed
    /// harness can branch on [`HarnessSource::Guess`].
    #[allow(dead_code)]
    pub harness_source: HarnessSource,
}

impl Identity {
    /// A stderr note when the result is a guess the operator should know
    /// about; `None` when the identity is firm.
    pub fn caveat(&self) -> Option<String> {
        match self.source {
            Source::LegacyFile => Some(
                "note: session id came from the legacy global current-session file \
                 (last-writer-wins across concurrent sessions) — export KB_SESSION_ID \
                 or pass --session-id for exact attribution"
                    .to_string(),
            ),
            Source::None => Some(
                "note: no session id resolved — nothing will be stamped with a session \
                 (slate posts show as unattributed)"
                    .to_string(),
            ),
            _ => None,
        }
    }
}

/// Everything the ladder reads, injected.
pub struct Inputs<'a> {
    pub flag_session: Option<&'a str>,
    pub flag_harness: Option<&'a str>,
    /// Environment lookup (`std::env::var(..).ok()` in production).
    pub env: &'a dyn Fn(&str) -> Option<String>,
    /// Raw contents of `current-session-repo-<slug>` (2 lines: id, unix ts).
    pub repo_marker: Option<&'a str>,
    /// Raw contents of the legacy global `current-session` file.
    pub legacy_marker: Option<&'a str>,
    pub now: i64,
}

fn pick(s: Option<&str>) -> Option<String> {
    let t = s?.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// The pure ladder.
pub fn resolve(i: &Inputs<'_>) -> Identity {
    let env = |k: &str| pick((i.env)(k).as_deref());
    let mut env_harness: Option<&'static str> = None;
    let (session_id, source) = if let Some(s) = pick(i.flag_session) {
        (Some(s), Source::Flag)
    } else if let Some(s) = env("KB_SESSION_ID") {
        (Some(s), Source::KbSessionId)
    } else if let Some(s) = env("CLAUDE_CODE_SESSION_ID") {
        env_harness = Some("claude");
        (Some(s), Source::ClaudeEnv)
    } else if let Some(s) = env("GROK_SESSION_ID") {
        env_harness = Some("grok");
        (Some(s), Source::GrokEnv)
    } else if let Some((sid, _)) = i
        .repo_marker
        .and_then(crate::commands::doctor::parse_repo_marker)
        .filter(|(_, ts)| {
            crate::commands::doctor::marker_is_fresh(
                i.now,
                *ts,
                crate::commands::doctor::REPO_MARKER_MAX_AGE_SECS,
            )
        })
    {
        (Some(sid), Source::RepoMarker)
    } else if let Some(s) = pick(i.legacy_marker) {
        (Some(s), Source::LegacyFile)
    } else {
        (None, Source::None)
    };

    let (harness, harness_source) = if let Some(h) = pick(i.flag_harness) {
        (h, HarnessSource::Flag)
    } else if let Some(h) = env("KB_HARNESS") {
        (h, HarnessSource::KbHarness)
    } else if let Some(h) = env_harness {
        (h.to_string(), HarnessSource::SessionEnv)
    } else {
        ("claude".to_string(), HarnessSource::Guess)
    };
    Identity {
        session_id,
        source,
        harness,
        harness_source,
    }
}

fn cache_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache")))
}

/// Production entry: reads the real env, the repo-keyed marker for `cwd`
/// (slug = the git toplevel's path, exactly `kb_slugify` in the hooks) and
/// the legacy marker.
pub fn resolve_process(
    flag_session: Option<&str>,
    flag_harness: Option<&str>,
    cwd: &Path,
) -> Identity {
    let cache = cache_dir();
    let repo_marker = cache.as_ref().and_then(|c| {
        let top = std::process::Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(["rev-parse", "--show-toplevel"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| cwd.to_string_lossy().to_string());
        let slug = crate::commands::doctor::slugify_repo_path(&top);
        if slug.is_empty() {
            return None;
        }
        std::fs::read_to_string(c.join("kb").join(format!("current-session-repo-{slug}"))).ok()
    });
    let legacy = cache
        .as_deref()
        .and_then(crate::session_marker::read_session_marker_at);
    resolve(&Inputs {
        flag_session,
        flag_harness,
        env: &|k| std::env::var(k).ok(),
        repo_marker: repo_marker.as_deref(),
        legacy_marker: legacy.as_deref(),
        now: chrono::Utc::now().timestamp(),
    })
}

/// Convenience for writers that only need the session id (remember, notes,
/// desk): resolves against the current directory and prints the caveat to
/// stderr when the id is a guess.
pub fn session_for_write(flag_session: Option<&str>) -> Option<String> {
    let cwd = std::env::current_dir().unwrap_or_default();
    let id = resolve_process(flag_session, None, &cwd);
    if let Some(c) = id.caveat() {
        eprintln!("{c}");
    }
    id.session_id
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const NOW: i64 = 1_700_000_000;

    fn run(
        flag: Option<&str>,
        env: &[(&str, &str)],
        repo: Option<&str>,
        legacy: Option<&str>,
    ) -> Identity {
        let m: HashMap<String, String> = env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let get = move |k: &str| m.get(k).cloned();
        resolve(&Inputs {
            flag_session: flag,
            flag_harness: None,
            env: &get,
            repo_marker: repo,
            legacy_marker: legacy,
            now: NOW,
        })
    }

    fn fresh() -> String {
        format!("repo-sid\n{}\n", NOW - 60)
    }

    #[test]
    fn flag_beats_everything() {
        let r = fresh();
        let id = run(
            Some("flag"),
            &[
                ("KB_SESSION_ID", "kb"),
                ("CLAUDE_CODE_SESSION_ID", "cc"),
                ("GROK_SESSION_ID", "gk"),
            ],
            Some(&r),
            Some("legacy"),
        );
        assert_eq!(id.session_id.as_deref(), Some("flag"));
        assert_eq!(id.source, Source::Flag);
    }

    #[test]
    fn kb_session_id_beats_harness_vars() {
        let id = run(
            None,
            &[("KB_SESSION_ID", "kb"), ("CLAUDE_CODE_SESSION_ID", "cc")],
            None,
            Some("legacy"),
        );
        assert_eq!(id.session_id.as_deref(), Some("kb"));
        assert_eq!(id.source, Source::KbSessionId);
    }

    #[test]
    fn claude_env_beats_grok_and_sets_harness() {
        let id = run(
            None,
            &[("CLAUDE_CODE_SESSION_ID", "cc"), ("GROK_SESSION_ID", "gk")],
            None,
            None,
        );
        assert_eq!(id.session_id.as_deref(), Some("cc"));
        assert_eq!(id.harness, "claude");
        assert_eq!(id.harness_source, HarnessSource::SessionEnv);
    }

    #[test]
    fn grok_env_names_grok_harness() {
        let id = run(None, &[("GROK_SESSION_ID", "gk")], None, Some("legacy"));
        assert_eq!(id.session_id.as_deref(), Some("gk"));
        assert_eq!(id.source, Source::GrokEnv);
        assert_eq!(id.harness, "grok");
    }

    #[test]
    fn fresh_repo_marker_beats_legacy_file() {
        let r = fresh();
        let id = run(None, &[], Some(&r), Some("legacy"));
        assert_eq!(id.session_id.as_deref(), Some("repo-sid"));
        assert_eq!(id.source, Source::RepoMarker);
    }

    #[test]
    fn stale_or_future_repo_marker_is_skipped() {
        let stale = format!("repo-sid\n{}\n", NOW - 2401);
        let id = run(None, &[], Some(&stale), Some("legacy"));
        assert_eq!(id.source, Source::LegacyFile);
        let future = format!("repo-sid\n{}\n", NOW + 500);
        assert_eq!(run(None, &[], Some(&future), None).source, Source::None);
        // malformed ts parses as 0 => maximally stale.
        assert_eq!(
            run(None, &[], Some("repo-sid\nabc\n"), None).source,
            Source::None
        );
    }

    #[test]
    fn legacy_file_is_flagged_fallback() {
        let id = run(None, &[], None, Some("  legacy-sid \n"));
        assert_eq!(id.session_id.as_deref(), Some("legacy-sid"));
        assert_eq!(id.source, Source::LegacyFile);
        assert!(id.caveat().unwrap().contains("legacy"));
    }

    #[test]
    fn nothing_resolves_to_none_with_caveat() {
        let id = run(None, &[], None, None);
        assert_eq!(id.session_id, None);
        assert_eq!(id.source, Source::None);
        assert!(id.caveat().unwrap().contains("unattributed"));
    }

    #[test]
    fn blank_values_fall_through() {
        let id = run(
            Some("  "),
            &[("KB_SESSION_ID", ""), ("CLAUDE_CODE_SESSION_ID", " ")],
            None,
            None,
        );
        assert_eq!(id.source, Source::None);
    }

    #[test]
    fn harness_ladder_flag_then_kb_harness_then_session_env_then_guess() {
        let m: HashMap<String, String> = [("KB_HARNESS", "omp"), ("CLAUDE_CODE_SESSION_ID", "cc")]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let get = move |k: &str| m.get(k).cloned();
        let mk = |fh: Option<&'static str>| {
            resolve(&Inputs {
                flag_session: None,
                flag_harness: fh,
                env: &get,
                repo_marker: None,
                legacy_marker: None,
                now: NOW,
            })
        };
        assert_eq!(mk(Some("codex")).harness, "codex");
        assert_eq!(mk(None).harness, "omp");
        assert_eq!(mk(None).harness_source, HarnessSource::KbHarness);
        // no KB_HARNESS: session env decides
        assert_eq!(
            run(None, &[("GROK_SESSION_ID", "g")], None, None).harness,
            "grok"
        );
        // nothing: flagged guess
        let g = run(None, &[], None, None);
        assert_eq!(g.harness, "claude");
        assert_eq!(g.harness_source, HarnessSource::Guess);
    }
}
