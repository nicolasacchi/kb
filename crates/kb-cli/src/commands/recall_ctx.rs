//! SG-Q2 — `--transcript` / `--turns` for `kb recall` and `kb turn`: the
//! recall query is composed CLIENT-SIDE from the current prompt plus the
//! recent turns of a live transcript, and sent as the ordinary `q` / prompt
//! parameter (no server change). Transcript text is unscrubbed mid-flight
//! content (invariant #11 live-lane posture), so it never leaves the machine:
//! a non-loopback daemon URL ignores `--transcript` with one stderr line.

use kb_core::sessions::{
    compose_recall_query, recent_turns_with, RecentTurnsOpts, RECALL_QUERY_DEFAULT_TOTAL_CHARS,
};
use std::net::IpAddr;
use std::path::Path;

/// Where a daemon URL with no `--daemon` points (mirrors `http::detect_daemon`).
pub const DEFAULT_DAEMON_URL: &str = "http://127.0.0.1:4000";

/// True iff `url`'s host is loopback: 127.0.0.0/8, `::1`, or the literal
/// `localhost`. A URL with no parseable host is NOT loopback (fail closed).
pub fn is_loopback_url(url: &str) -> bool {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let hostport = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if let Some(r) = hostport.strip_prefix('[') {
        match r.split_once(']') {
            Some((h, _)) => h,
            None => return false,
        }
    } else {
        match hostport.rsplit_once(':') {
            // a bare IPv6 literal has several colons and no brackets: not a URL host
            Some((h, p)) if !h.contains(':') && p.chars().all(|c| c.is_ascii_digit()) => h,
            Some(_) => return false,
            None => hostport,
        }
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
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
            "localhost:4000",
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
