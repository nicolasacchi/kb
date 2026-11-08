//! Restart-persistent per-session state, in tmux user options on the
//! per-session holder (like [`grid::OWNER_OPTION`](crate::runner::grid::OWNER_OPTION),
//! the grid owner): a runner restart closes every pty master, so the
//! in-memory viewer registry and `paste_only` flag alone would drop
//! both T9 guards after restart (viewers gone, flag forgotten). These
//! options are the durable record [`adopt_anchors`](crate::runner::adopt_anchors)
//! replays: re-opening the recorded viewers best-effort, and reading
//! `paste_only` lazily on first use after a restart.
//!
//! The viewer set is a registration, not a teardown event: closing
//! viewers because the anchor closed (spec 5) leaves the option
//! untouched, and a runner shutdown leaves it intact, so a restart
//! re-opens the viewers a user opened. Only an explicit `viewer_close`
//! deregisters (rewrites the option).

use std::path::Path;

use crate::runner::tmux::Tmux;
use crate::runner::RunnerError;

/// The `paste_only` option key ("1" = set, "0"/unset = clear).
pub const PASTE_ONLY_OPTION: &str = "@kb_paste_only";
/// The viewer-set option key (comma-joined viewer ids).
pub const VIEWERS_OPTION: &str = "@kb_viewers";

/// Encode the viewer set (sorted for determinism; invalid ids dropped).
pub fn encode_viewers(viewers: &[String]) -> String {
    let mut sorted: Vec<&str> = viewers
        .iter()
        .map(String::as_str)
        .filter(|v| crate::viewer::valid_viewer(v))
        .collect();
    sorted.sort();
    sorted.dedup();
    sorted.join(",")
}

/// Decode an `@kb_viewers` value; invalid/empty entries are dropped,
/// the result is sorted and deduplicated.
pub fn decode_viewers(raw: &str) -> Vec<String> {
    let mut ids: Vec<&str> = raw
        .split(',')
        .map(str::trim)
        .filter(|v| !v.is_empty() && crate::viewer::valid_viewer(v))
        .collect();
    ids.sort();
    ids.dedup();
    ids.into_iter().map(ToString::to_string).collect()
}

/// Read the durable `paste_only` flag (`false` when unset/unreadable —
/// the failure mode before any paste, and when the holder is absent).
pub async fn read_paste_only<T: Tmux>(tmux: &T, sock: &Path) -> bool {
    let Ok(out) = tmux
        .run(sock, &["show-options", "-gqv", PASTE_ONLY_OPTION], None)
        .await
    else {
        return false;
    };
    out.stdout.trim() == "1"
}

/// Write the durable `paste_only` flag.
pub async fn write_paste_only<T: Tmux>(
    tmux: &T,
    sock: &Path,
    value: bool,
) -> Result<(), RunnerError> {
    let out = tmux
        .run(
            sock,
            &[
                "set-option",
                "-g",
                PASTE_ONLY_OPTION,
                if value { "1" } else { "0" },
            ],
            None,
        )
        .await?;
    if !out.ok {
        return Err(RunnerError::TmuxFailed(format!(
            "set-option {PASTE_ONLY_OPTION} failed: {}",
            out.stderr.trim()
        )));
    }
    Ok(())
}

/// Read the durable viewer set (`[]` when unset/unreadable).
pub async fn read_viewers<T: Tmux>(tmux: &T, sock: &Path) -> Vec<String> {
    let Ok(out) = tmux
        .run(sock, &["show-options", "-gqv", VIEWERS_OPTION], None)
        .await
    else {
        return Vec::new();
    };
    decode_viewers(&out.stdout)
}

/// Write the durable viewer set.
pub async fn write_viewers<T: Tmux>(
    tmux: &T,
    sock: &Path,
    viewers: &[String],
) -> Result<(), RunnerError> {
    let encoded = encode_viewers(viewers);
    let out = tmux
        .run(sock, &["set-option", "-g", VIEWERS_OPTION, &encoded], None)
        .await?;
    if !out.ok {
        return Err(RunnerError::TmuxFailed(format!(
            "set-option {VIEWERS_OPTION} failed: {}",
            out.stderr.trim()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::tmux::fake::FakeTmux;

    fn test_sock() -> std::path::PathBuf {
        std::path::PathBuf::from("/tmp/fake.sock")
    }

    #[test]
    fn viewers_codec_round_trips_and_drops_garbage() {
        let encoded = encode_viewers(&["web".to_string(), "spa".to_string(), "web".to_string()]);
        assert_eq!(encoded, "spa,web");
        assert_eq!(decode_viewers(&encoded), vec!["spa", "web"]);
        assert_eq!(decode_viewers(""), Vec::<String>::new());
        assert_eq!(
            decode_viewers("web,,KV,spa"),
            vec!["spa", "web"],
            "uppercase is not a valid viewer id and must be dropped"
        );
        assert_eq!(encode_viewers(&[]), "");
    }

    #[tokio::test]
    async fn options_round_trip_through_the_fake() {
        // paste_only: default false, then "1" after write, back to
        // false after clearing (the fake tracks only @kb_owner_grid;
        // the generic canned script serves these options).
        let fake = FakeTmux::new();
        let sock = test_sock();
        assert!(!read_paste_only(&fake, &sock).await);
        write_paste_only(&fake, &sock, true).await.unwrap();
        let calls = fake.calls.lock().unwrap();
        let written = calls
            .iter()
            .find(|(_, a)| a.first().map(String::as_str) == Some("set-option"));
        assert_eq!(
            written.map(|(_, a)| a.clone()),
            Some(vec![
                "set-option".to_string(),
                "-g".to_string(),
                PASTE_ONLY_OPTION.to_string(),
                "1".to_string()
            ]),
            "paste_only writes the documented option/value"
        );
        drop(calls);
        fake.canned
            .lock()
            .unwrap()
            .insert("show-options".to_string(), "1\n".to_string());
        assert!(read_paste_only(&fake, &sock).await);
        fake.canned
            .lock()
            .unwrap()
            .insert("show-options".to_string(), "0\n".to_string());
        assert!(!read_paste_only(&fake, &sock).await);
        // viewers: written encoded, read back decoded and sorted.
        write_viewers(&fake, &sock, &["web".to_string(), "spa".to_string()])
            .await
            .unwrap();
        fake.canned
            .lock()
            .unwrap()
            .insert("show-options".to_string(), "web,spa\n".to_string());
        assert_eq!(read_viewers(&fake, &sock).await, vec!["spa", "web"]);
        // A missing holder (tmux error) reads as empty, never as an
        // error: adoption before any holder exists must be a no-op.
        let dead = FakeTmux {
            timeout: true,
            ..FakeTmux::new()
        };
        assert!(!read_paste_only(&dead, &sock).await);
        assert!(read_viewers(&dead, &sock).await.is_empty());
    }
}
