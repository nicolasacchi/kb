//! Paste policy: bracketed-aware, validated, server-side.
//!
//! The paste goes through the tmux server (`load-buffer` +
//! `paste-buffer -p -r -S -d`), never through the hidden client. `-r` is
//! mandatory: tmux replaces LF with CR by default even with `-p`, which
//! would turn a bracketed multi-line paste into Enter keys. `-S` stops
//! vis(3) sanitising (the text is already validated here). Multi-line
//! text is refused when the pane has bracketed paste off, because the LF
//! would then act as Enter. A `ESC[201~` smuggled into the text is
//! stopped by rejecting C0 controls except `\t` and `\n`, plus DEL.
//!
//! Never log the pasted text.

use std::path::Path;

use crate::runner::tmux::Tmux;
use crate::runner::{Runner, RunnerError};

/// Validate the text (size, then control bytes).
pub fn check_text(text: &str, max_bytes: usize) -> Result<(), RunnerError> {
    if text.is_empty() {
        return Err(RunnerError::TooLarge("paste text is empty".to_string()));
    }
    if text.len() > max_bytes {
        return Err(RunnerError::TooLarge(format!(
            "paste text exceeds {max_bytes} bytes"
        )));
    }
    for &b in text.as_bytes() {
        match b {
            b'\t' | b'\n' => {}
            0x7F => return Err(RunnerError::BadText("paste contains DEL".to_string())),
            0x00..=0x1F => {
                return Err(RunnerError::BadText(format!(
                    "paste contains control byte 0x{b:02x}"
                )))
            }
            _ => {}
        }
    }
    Ok(())
}

/// Is this paste `paste_only` (starts with `/` or `@`)?
pub fn is_paste_only(text: &str) -> bool {
    text.starts_with('/') || text.starts_with('@')
}
async fn pane_flag_and_id<T: Tmux>(tmux: &T, sock: &Path) -> Result<(bool, String), RunnerError> {
    let out = tmux
        .run(
            sock,
            &[
                "display-message",
                "-p",
                "-t",
                "kb",
                "#{bracket_paste_flag} #{pane_id}",
            ],
            None,
        )
        .await?;
    if !out.ok {
        return Err(RunnerError::NotFound(format!(
            "no pane in {}",
            sock.display()
        )));
    }
    let mut parts = out.stdout.split_whitespace();
    let bracketed = parts.next() == Some("1");
    let pane = parts.next().unwrap_or("kb").to_string();
    Ok((bracketed, pane))
}

/// The outcome of a successful paste.
#[derive(Debug, Clone, Copy)]
pub struct PasteOutcome {
    /// Whether bracketed paste was on in the pane.
    pub bracketed: bool,
    /// Whether the text starts with `/` or `@` (composer: Enter suppressed).
    pub paste_only: bool,
}

/// Paste `text` into the session's pane via the tmux server.
pub async fn paste_text<S, T>(
    runner: &Runner<S>,
    tmux: &T,
    id: &str,
    text: &str,
) -> Result<PasteOutcome, RunnerError>
where
    S: crate::runner::supervisor::Supervisor,
    T: Tmux,
{
    paste_with(
        tmux,
        &runner.holder_socket(id),
        id,
        text,
        runner.cfg.paste_max_bytes,
        runner.next_buffer_seq(id).await,
    )
    .await
    .map(|outcome| {
        // Recorded by the caller via the runner's per-session flag after
        // success; the tee to the runner keeps this one canonical.
        outcome
    })
}

/// Validated paste against any [`Tmux`] (real or fake); the RPC records
/// the `paste_only` flag after success.
pub async fn paste_with<T: Tmux>(
    tmux: &T,
    sock: &Path,
    id: &str,
    text: &str,
    max_bytes: usize,
    seq: u64,
) -> Result<PasteOutcome, RunnerError> {
    check_text(text, max_bytes)?;
    let (bracketed, pane) = pane_flag_and_id(tmux, sock).await?;
    if text.contains('\n') && !bracketed {
        return Err(RunnerError::MultiLineRefused(
            "multi-line paste refused: bracketed paste is off in the pane".to_string(),
        ));
    }
    let name = format!("kbterm-{id}-{seq}");
    let loaded = tmux
        .run_with_stdin(
            sock,
            &["load-buffer", "-b", &name, "-"],
            Some(text.as_bytes()),
        )
        .await;
    if let Err(e) = loaded {
        let _ = tmux.run(sock, &["delete-buffer", "-b", &name], None).await;
        return Err(e);
    }
    let out = tmux
        .run(
            sock,
            &[
                "paste-buffer",
                "-p",
                "-r",
                "-S",
                "-d",
                "-b",
                &name,
                "-t",
                &pane,
            ],
            None,
        )
        .await;
    // The buffer is a named scratch space; delete it on every path.
    let _ = tmux.run(sock, &["delete-buffer", "-b", &name], None).await;
    let out = out?;
    if !out.ok {
        return Err(RunnerError::TmuxFailed(format!(
            "paste-buffer failed: {}",
            out.stderr.trim()
        )));
    }
    Ok(PasteOutcome {
        bracketed,
        paste_only: is_paste_only(text),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::tmux::fake::FakeTmux;

    fn sock() -> std::path::PathBuf {
        std::path::PathBuf::from("/tmp/fake.sock")
    }

    fn fake_with(bracketed: bool) -> FakeTmux {
        let fake = FakeTmux::new();
        let ans = if bracketed { "1 %5\n" } else { "0 %5\n" };
        fake.canned
            .lock()
            .unwrap()
            .insert("display-message".to_string(), ans.to_string());
        fake
    }

    fn args(fake: &FakeTmux) -> Vec<Vec<String>> {
        fake.calls
            .lock()
            .unwrap()
            .iter()
            .map(|(_, a)| a.clone())
            .collect()
    }

    #[tokio::test]
    async fn paste_policy_table() {
        // Single-line, bracketed off: raw paste, no flags.
        let fake = fake_with(false);
        let out = paste_with(&fake, &sock(), "abcd1234", "ab", 65536, 1)
            .await
            .unwrap();
        assert_eq!(out.bracketed, false);
        assert_eq!(out.paste_only, false);
        // Multi-line, bracketed off: refused, nothing pasted (no
        // load-buffer call).
        let fake = fake_with(false);
        assert_eq!(
            paste_with(&fake, &sock(), "abcd1234", "a\nb", 65536, 1)
                .await
                .unwrap_err()
                .code(),
            "multi-line-refused"
        );
        assert!(
            args(&fake)
                .iter()
                .all(|a| a.first().map(String::as_str) != Some("load-buffer")),
            "refused paste must not load a buffer"
        );
        // Multi-line, bracketed on: pasted, LF preserved by -r.
        let fake = fake_with(true);
        let out = paste_with(&fake, &sock(), "abcd1234", "a\nb", 65536, 1)
            .await
            .unwrap();
        assert!(out.bracketed);
        let argv = args(&fake);
        let load = argv
            .iter()
            .find(|a| a.first().map(String::as_str) == Some("load-buffer"))
            .expect("load-buffer must be called");
        assert_eq!(load, &vec!["load-buffer", "-b", "kbterm-abcd1234-1", "-"]);
        let paste = argv
            .iter()
            .find(|a| a.first().map(String::as_str) == Some("paste-buffer"))
            .expect("paste-buffer must be called");
        assert_eq!(
            paste,
            &vec![
                "paste-buffer",
                "-p",
                "-r",
                "-S",
                "-d",
                "-b",
                "kbterm-abcd1234-1",
                "-t",
                "%5"
            ]
        );
        // Control bytes and DEL -> bad-text; tab is fine.
        for bad in ["a\x1bb", "a\x00b", "a\x7fb", "\x1b[201~"] {
            let fake = fake_with(true);
            assert_eq!(
                paste_with(&fake, &sock(), "abcd1234", bad, 65536, 1)
                    .await
                    .unwrap_err()
                    .code(),
                "bad-text",
                "text {bad:?} must be bad-text"
            );
        }
        let fake = fake_with(true);
        paste_with(&fake, &sock(), "abcd1234", "a\tb", 65536, 1)
            .await
            .expect("tab must be accepted");
        // Over the cap -> too-large.
        let fake = fake_with(true);
        assert_eq!(
            paste_with(
                &fake,
                &sock(),
                "abcd1234",
                "xxxxxxxxxxxxxxxxxxxxxxxxxx",
                8,
                1
            )
            .await
            .unwrap_err()
            .code(),
            "too-large"
        );
        // paste_only: `/` or `@` start; plain text is not.
        for (text, want) in [
            ("/help", true),
            ("@file", true),
            ("help", false),
            ("x/y", false),
        ] {
            let fake = fake_with(true);
            let out = paste_with(&fake, &sock(), "abcd1234", text, 65536, 1)
                .await
                .unwrap();
            assert_eq!(out.paste_only, want, "text {text:?}");
        }
        // Empty text -> too-large.
        let fake = fake_with(true);
        assert_eq!(
            paste_with(&fake, &sock(), "abcd1234", "", 65536, 1)
                .await
                .unwrap_err()
                .code(),
            "too-large"
        );
    }
}
