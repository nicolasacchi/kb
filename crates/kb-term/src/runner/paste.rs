//! Paste policy: bracketed-aware, validated, server-side.
//!
//! The paste goes through the tmux server (`load-buffer` +
//! `paste-buffer -p -r -S -d`), never through the hidden client, and in ONE
//! tmux client process (the steps are chained with `\;`). `-r` is
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
}

/// Validated paste against any [`Tmux`] (real or fake); the RPC records
/// the `paste_only` flag after success.
///
/// ONE tmux client process does everything: the pane's bracketed-paste
/// flag is printed first, the buffer is loaded from stdin, and the paste
/// is made (a multi-line one only when the flag is on, through `if-shell
/// -F`; otherwise the scratch buffer is deleted in the same invocation).
/// The text never enters argv: only fixed words, the pane target and the
/// scratch-buffer name do, and `paste-buffer -d` removes the buffer on
/// success. A spawn costs hundreds of ms on a loaded host, so the old
/// four-process sequence (flag, load, paste, delete) made every composer
/// send cost 0.7-1.3 s.
pub async fn paste_with<T: Tmux>(
    tmux: &T,
    sock: &Path,
    id: &str,
    text: &str,
    max_bytes: usize,
    seq: u64,
) -> Result<PasteOutcome, RunnerError> {
    check_text(text, max_bytes)?;
    let name = format!("kbterm-{id}-{seq}");
    let multi_line = text.contains('\n');
    let paste_cmd = format!("paste-buffer -p -r -S -d -b {name} -t kb");
    let drop_cmd = format!("delete-buffer -b {name}");
    let gate = "#{bracket_paste_flag}";
    let mut argv: Vec<&str> = vec![
        "display-message",
        "-p",
        gate,
        ";",
        "load-buffer",
        "-b",
        &name,
        "-",
        ";",
    ];
    if multi_line {
        argv.extend(["if-shell", "-F", gate, &paste_cmd, &drop_cmd]);
    } else {
        argv.extend([
            "paste-buffer",
            "-p",
            "-r",
            "-S",
            "-d",
            "-b",
            &name,
            "-t",
            "kb",
        ]);
    }
    let out = tmux
        .run_with_stdin(sock, &argv, Some(text.as_bytes()))
        .await;
    let out = match out {
        Ok(o) => o,
        Err(e) => {
            // The buffer is a named scratch space; never leave one behind.
            let _ = tmux.run(sock, &["delete-buffer", "-b", &name], None).await;
            return Err(e);
        }
    };
    let flag_seen = out.stdout.split_whitespace().next();
    if !out.ok {
        if flag_seen.is_none() {
            return Err(RunnerError::NotFound(format!(
                "no pane in {}",
                sock.display()
            )));
        }
        let _ = tmux.run(sock, &["delete-buffer", "-b", &name], None).await;
        return Err(RunnerError::TmuxFailed(format!(
            "paste-buffer failed: {}",
            out.stderr.trim()
        )));
    }
    let bracketed = flag_seen == Some("1");
    if multi_line && !bracketed {
        return Err(RunnerError::MultiLineRefused(
            "multi-line paste refused: bracketed paste is off in the pane".to_string(),
        ));
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
    async fn paste_costs_one_tmux_client_process_whatever_the_shape() {
        // Every spawn costs hundreds of ms on a loaded host: a paste is ONE
        // tmux invocation (flag + load + paste + cleanup chained), for a
        // single line, a bracketed multi-line paste and a refused one.
        for (text, bracketed) in [("ab", false), ("ab", true), ("a\nb", true), ("a\nb", false)] {
            let fake = fake_with(bracketed);
            let _ = paste_with(&fake, &sock(), "abcd1234", text, 65536, 1).await;
            assert_eq!(
                args(&fake).len(),
                1,
                "text {text:?} bracketed={bracketed}: {:?}",
                args(&fake)
            );
        }
    }

    #[tokio::test]
    async fn paste_chain_never_carries_the_text_in_argv() {
        // The text rides stdin (an argv text ending in `;` would split the
        // tmux command line); only fixed words and the scratch name are argv.
        let fake = fake_with(true);
        paste_with(&fake, &sock(), "abcd1234", "x;", 65536, 1)
            .await
            .unwrap();
        for a in args(&fake).iter().flatten() {
            assert!(!a.contains("x;"), "text leaked into argv: {a}");
        }
        assert_eq!(fake.stdin_log.lock().unwrap()[0], b"x;");
    }

    #[tokio::test]
    async fn paste_policy_table() {
        // Single-line, bracketed off: raw paste, no flags.
        let fake = fake_with(false);
        let out = paste_with(&fake, &sock(), "abcd1234", "ab", 65536, 1)
            .await
            .unwrap();
        assert!(!out.bracketed);
        assert!(!out.paste_only);
        // Multi-line, bracketed off: refused; the chained invocation
        // deletes the scratch buffer instead of pasting it.
        let fake = fake_with(false);
        assert_eq!(
            paste_with(&fake, &sock(), "abcd1234", "a\nb", 65536, 1)
                .await
                .unwrap_err()
                .code(),
            "multi-line-refused"
        );
        let argv = args(&fake);
        assert_eq!(argv.len(), 1);
        let ifs = argv[0].iter().position(|a| a == "if-shell").unwrap();
        assert_eq!(
            &argv[0][ifs..],
            &[
                "if-shell",
                "-F",
                "#{bracket_paste_flag}",
                "paste-buffer -p -r -S -d -b kbterm-abcd1234-1 -t kb",
                "delete-buffer -b kbterm-abcd1234-1"
            ]
        );
        // Multi-line, bracketed on: pasted, LF preserved by -r.
        let fake = fake_with(true);
        let out = paste_with(&fake, &sock(), "abcd1234", "a\nb", 65536, 1)
            .await
            .unwrap();
        assert!(out.bracketed);
        // The buffer bytes reach tmux on stdin verbatim (without this
        // assertion a paste that drops `text` entirely would pass).
        {
            let stdin = fake.stdin_log.lock().unwrap();
            assert_eq!(stdin.len(), 1, "exactly one load-buffer stdin");
            assert_eq!(stdin[0], b"a\nb", "stdin must be the text bytes");
        }
        let argv = args(&fake);
        assert_eq!(
            &argv[0][..9],
            &[
                "display-message",
                "-p",
                "#{bracket_paste_flag}",
                ";",
                "load-buffer",
                "-b",
                "kbterm-abcd1234-1",
                "-",
                ";"
            ]
        );
        // Single line: an unconditional paste-buffer with the full flags.
        let fake = fake_with(false);
        paste_with(&fake, &sock(), "abcd1234", "ab", 65536, 1)
            .await
            .unwrap();
        assert_eq!(
            &args(&fake)[0][9..],
            &[
                "paste-buffer",
                "-p",
                "-r",
                "-S",
                "-d",
                "-b",
                "kbterm-abcd1234-1",
                "-t",
                "kb"
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
