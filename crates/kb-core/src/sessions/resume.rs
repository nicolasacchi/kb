//! AH3 — the ONE per-harness resume-command table.
//!
//! Four producers drifted: live-status showed `omp resume <sid>` / `grok
//! resume <sid>` (neither exists), the grok live adapter said `grok
//! --resume`, the CLI called kimi/opencode not resumable while their
//! adapters printed real commands, and the sessions inspector showed
//! `claude -r` for every harness. Copy-only text: kb never runs it.

/// What every surface shows when the recorded harness name or session id is
/// not a plain token. Echoes neither (they are untrusted: a beat's id, a
/// transcript's adapter-meta harness).
pub const NOT_RESUMABLE_UNSAFE: &str =
    "not resumable from this machine (the recorded session id or harness name is not a plain token)";

/// A plain token: non-empty, at most 128 bytes, starts alphanumeric, and
/// only alphanumerics or `.` `_` `:` `-` after that.
fn is_plain_token(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.as_bytes()[0].is_ascii_alphanumeric()
        && s.as_bytes()
            .iter()
            .all(|&c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b':' | b'-'))
}

/// The `--cwd` argument for grok: absolute, no control characters, bare when
/// every byte is shell-plain, otherwise POSIX single-quoted.
fn grok_cwd_arg(cwd: Option<&str>) -> Option<String> {
    let c = cwd?;
    if !c.starts_with('/') || c.chars().any(char::is_control) {
        return None;
    }
    if c.as_bytes()
        .iter()
        .all(|&b| b.is_ascii_alphanumeric() || b"._/:+@%=,-".contains(&b))
    {
        Some(c.to_string())
    } else {
        Some(format!("'{}'", c.replace('\'', r"'\''")))
    }
}

/// The ONE place that knows how each harness resumes a session by id.
/// `cwd` is used only where the harness needs it (grok). Total over any
/// input: an unknown harness or an unsafe id/harness yields an honest
/// "not resumable" sentence, never a fabricated command.
pub fn resume_command(harness: &str, session_id: &str, cwd: Option<&str>) -> String {
    // A hook-pushed id, a transcript-recorded harness or a cwd must never
    // become shell metacharacters or a leading-dash flag in a copy-paste
    // command.
    if !is_plain_token(harness) || !is_plain_token(session_id) {
        return NOT_RESUMABLE_UNSAFE.to_string();
    }
    match harness {
        "claude" => format!("claude -r {session_id}"),
        // `codex resume <uuid>` — the verb exists (`codex --help`).
        "codex" => format!("codex resume {session_id}"),
        "grok" => match grok_cwd_arg(cwd) {
            Some(c) => format!("grok -r {session_id} --cwd {c}"),
            None => format!("grok -r {session_id}"),
        },
        "kimi" => format!("kimi -S {session_id}"),
        "opencode" => format!("opencode --session {session_id}"),
        // omp's `--resume` searches the current-cwd bucket then ALL sessions
        // globally, so no cwd is needed (unlike grok).
        "omp" => format!("omp -r {session_id}"),
        other => format!(
            "{other} — not resumable from this machine (no known {other} resume command; \
             session id: {session_id})"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::HARNESSES;

    /// Drift guard (the omp/placeholder incident): a harness added to the
    /// closed set without a table arm fails the loop.
    #[test]
    fn resume_command_matches_the_verified_cli_spelling_per_harness() {
        assert_eq!(resume_command("claude", "sid1", None), "claude -r sid1");
        assert_eq!(resume_command("codex", "sid1", None), "codex resume sid1");
        assert_eq!(resume_command("grok", "sid1", None), "grok -r sid1");
        assert_eq!(resume_command("kimi", "sid1", None), "kimi -S sid1");
        assert_eq!(
            resume_command("opencode", "sid1", None),
            "opencode --session sid1"
        );
        assert_eq!(resume_command("omp", "sid1", None), "omp -r sid1");
        for h in HARNESSES {
            let out = resume_command(h, "sid1", None);
            assert!(!out.contains("not resumable"), "{h}: {out}");
        }
    }

    #[test]
    fn grok_resume_adds_cwd_only_when_absolute_and_quotes_anything_not_plain() {
        for c in [
            None,
            Some(""),
            Some("rel/dir"),
            Some("/w/x\n"),
            Some("/w/\0x"),
        ] {
            assert_eq!(resume_command("grok", "sid1", c), "grok -r sid1", "{c:?}");
        }
        assert_eq!(
            resume_command("grok", "sid1", Some("/w/x")),
            "grok -r sid1 --cwd /w/x"
        );
        assert_eq!(
            resume_command("grok", "sid1", Some("/w/my dir")),
            "grok -r sid1 --cwd '/w/my dir'"
        );
        assert_eq!(
            resume_command("grok", "sid1", Some("/w/it's")),
            r#"grok -r sid1 --cwd '/w/it'\''s'"#
        );
        assert_eq!(
            resume_command("grok", "sid1", Some("/w/$(id)")),
            "grok -r sid1 --cwd '/w/$(id)'"
        );
    }

    #[test]
    fn cwd_is_ignored_for_every_harness_except_grok() {
        for h in ["claude", "codex", "kimi", "opencode", "omp"] {
            assert_eq!(
                resume_command(h, "sid1", Some("/w/x")),
                resume_command(h, "sid1", None),
                "{h}"
            );
        }
    }

    #[test]
    fn a_session_id_or_harness_that_is_not_a_plain_token_is_never_a_command() {
        for h in HARNESSES {
            for id in [
                "",
                "a b",
                "x;rm -rf ~",
                "$(id)",
                "-rf",
                "--dangerously-skip-permissions",
                "a\nb",
            ] {
                assert_eq!(
                    resume_command(h, id, None),
                    NOT_RESUMABLE_UNSAFE,
                    "{h} {id:?}"
                );
            }
        }
        for h in ["", "a b", "$(id)"] {
            assert_eq!(
                resume_command(h, "sid1", None),
                NOT_RESUMABLE_UNSAFE,
                "{h:?}"
            );
        }
    }

    #[test]
    fn unknown_plain_harness_gets_the_honest_fallback() {
        assert_eq!(
            resume_command("kimi-x", "sid1", None),
            "kimi-x — not resumable from this machine (no known kimi-x resume command; session id: sid1)"
        );
    }
}
