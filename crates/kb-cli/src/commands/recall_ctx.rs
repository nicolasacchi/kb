//! SG-Q2 — `--transcript` / `--turns` for `kb recall` and `kb turn`: the
//! recall query is composed CLIENT-SIDE from the current prompt plus the
//! recent turns of a live transcript, and sent as the ordinary `q` / prompt
//! parameter (no server change). Transcript text is unscrubbed mid-flight
//! content (invariant #11 live-lane posture), so it never leaves the machine:
//! a non-loopback daemon URL ignores `--transcript` with one stderr line.

use kb_core::sessions::{
    compose_recall_query, recent_turns_with, RecentTurnsOpts, RECALL_QUERY_DEFAULT_TOTAL_CHARS,
};
use std::path::Path;

/// Where a daemon URL with no `--daemon` points (mirrors `http::detect_daemon`).
pub const DEFAULT_DAEMON_URL: &str = "http://127.0.0.1:4000";

/// True iff `url` is an http(s) URL whose PARSED host is loopback: 127.0.0.0/8,
/// `::1`, or the literal `localhost`. Fails closed: anything unparseable, any
/// other scheme, any backslash (WHATWG treats `\\` as `/` in special URLs,
/// so a hand-split authority and the HTTP client disagree on the host) and
/// any userinfo is NOT loopback.
pub fn is_loopback_url(url: &str) -> bool {
    if url.contains('\\') {
        return false;
    }
    let Ok(u) = reqwest::Url::parse(url.trim()) else {
        return false;
    };
    if !matches!(u.scheme(), "http" | "https") {
        return false;
    }
    if !u.username().is_empty() || u.password().is_some() {
        return false;
    }
    let Some(host) = u.host_str() else {
        return false;
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    bare.parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
}

/// The query to send: `prompt` unchanged unless `transcript` is given, the
/// daemon is loopback and the transcript yields turns.
pub fn query_with_context(
    prompt: &str,
    transcript: Option<&str>,
    turns: usize,
    daemon: Option<&str>,
) -> String {
    let Some(path) = transcript else {
        return prompt.to_string();
    };
    let url = daemon.unwrap_or(DEFAULT_DAEMON_URL);
    if !is_loopback_url(url) {
        eprintln!(
            "kb: --transcript ignored: daemon {url} is not loopback (transcript text is unscrubbed); sending the prompt alone"
        );
        return prompt.to_string();
    }
    let opts = RecentTurnsOpts {
        max_turns: turns,
        ..RecentTurnsOpts::default()
    };
    let t = recent_turns_with(Path::new(path), &opts);
    compose_recall_query(prompt, &t, RECALL_QUERY_DEFAULT_TOTAL_CHARS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_classification() {
        for u in [
            "http://127.0.0.1:4000",
            "http://127.1.2.3:80/x",
            "http://[::1]:4000",
            "http://[::1]",
            "http://localhost:4000/",
            "http://LOCALHOST",
            "https://127.0.0.1:4000",
        ] {
            assert!(is_loopback_url(u), "{u}");
        }
        for u in [
            "http://10.0.0.5:4000",
            "http://192.168.1.2",
            "https://kb.example.com",
            "http://localhost.evil.com:4000",
            "http://127.0.0.1.evil.com",
            "http://[2001:db8::1]:4000",
            "http://user@evil.com:4000/127.0.0.1",
            "http://[::1",
            "",
            // bypass: the client connects to evil.com, a hand split saw 127.0.0.1
            "http://evil.com\\@127.0.0.1:4000",
            "http://127.0.0.1\\@evil.com",
            // userinfo, even with a loopback host
            "http://user@127.0.0.1:4000",
            "http://user:pw@localhost:4000",
            // non-http schemes and scheme-less input
            "ftp://127.0.0.1",
            "file:///etc/passwd",
            "localhost:4000",
            "127.0.0.1:4000",
        ] {
            assert!(!is_loopback_url(u), "{u}");
        }
    }

    #[test]
    fn no_transcript_or_remote_daemon_is_the_prompt_alone() {
        assert_eq!(query_with_context("p  q", None, 4, None), "p  q");
        assert_eq!(
            query_with_context("p", Some("/nonexistent"), 4, Some("http://10.1.1.1:4000")),
            "p"
        );
        // loopback + unreadable file: no turns => unchanged
        assert_eq!(
            query_with_context("p  q", Some("/nonexistent"), 4, None),
            "p  q"
        );
    }
}
