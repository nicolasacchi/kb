//! Public-mirror posture check (v0.45 N9). A pure function over a loaded
//! [`KbConfig`] that reports where a `kb.toml` falls short of the posture
//! `docs/public-mirror.md` requires of a dedicated public-mirror daemon.
//!
//! This is a LINT of a deployment recipe, not a daemon feature: kb gains no
//! public mode (README Non-goals) and the daemon never runs this. It reads
//! only the config file, so what it cannot see -- the edge rules, the
//! `KB_ALLOW_NO_AUTH` environment, what the mounted directories contain --
//! stays the operator's checklist in the doc. Every finding has a stable
//! `rule` id; the doc's rule table and the unit tests are keyed on them.

use crate::config::KbConfig;
use std::net::IpAddr;

/// One way a config departs from the public-mirror posture.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Finding {
    /// Stable rule id (kebab-case), listed in docs/public-mirror.md.
    pub rule: &'static str,
    /// Config location the finding is about (`[kb.x.outbound]`, `[server]`).
    pub subject: String,
    /// What is wrong and what to set instead.
    pub message: String,
}

/// Every rule id this module can emit, in the order the doc lists them.
pub const RULES: &[&str] = &[
    "no-corpus",
    "strip-kb-prompt",
    "hostnames-unset",
    "artifact-origin-default",
    "bind-not-private",
    "private-corpus",
    "capture-lane",
    "sessions-live-dir",
    "webhook-egress",
    "home-path",
];

fn f(rule: &'static str, subject: impl Into<String>, message: impl Into<String>) -> Finding {
    Finding {
        rule,
        subject: subject.into(),
        message: message.into(),
    }
}

/// True for loopback and private-range (RFC 1918 / ULA / link-local) hosts.
fn host_is_private(host: &str) -> bool {
    let h = host.trim_start_matches('[').trim_end_matches(']');
    match h.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        Ok(IpAddr::V6(v6)) => {
            let seg0 = v6.segments()[0];
            v6.is_loopback() || (seg0 & 0xfe00) == 0xfc00 || (seg0 & 0xffc0) == 0xfe80
        }
        Err(_) => h.eq_ignore_ascii_case("localhost"),
    }
}

fn addr_host(addr: &str) -> &str {
    match addr.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) => h,
        _ => addr,
    }
}

/// A deliberately conservative PREFIX heuristic: a name such as
/// `memory-bank-docs` is flagged too. There is no per-rule silence; rename the
/// corpus (or its `default_search_category`) to something neutral.
fn is_private_name(s: &str) -> bool {
    let s = s.to_ascii_lowercase();
    s.starts_with("memory") || s.starts_with("session")
}

/// Check `cfg` against the public-mirror posture. Empty = clean.
pub fn public_mirror_findings(cfg: &KbConfig) -> Vec<Finding> {
    let mut out = Vec::new();

    if cfg.kb.is_empty() {
        out.push(f(
            "no-corpus",
            "[kb.*]",
            "no corpus is configured; a mirror serves at least one public corpus",
        ));
    }

    for (name, kb) in &cfg.kb {
        let strips = kb.outbound.as_ref().is_some_and(|o| o.strip_kb_prompt);
        if !strips {
            out.push(f(
                "strip-kb-prompt",
                format!("[kb.{name}.outbound]"),
                "set `strip_kb_prompt = true`; without it artifact bytes carry the kb-prompt template",
            ));
        }
        let cat = kb.default_search_category.as_deref().unwrap_or("");
        if is_private_name(&name.to_string())
            || is_private_name(cat)
            || kb.memory_scope.is_some()
            || kb.decay_policy.is_some()
        {
            out.push(f(
                "private-corpus",
                format!("[kb.{name}]"),
                "looks like a memory or sessions corpus (name, default_search_category, memory_scope or decay_policy); never mount one on a mirror",
            ));
        }
        if kb.capture_dir.is_some() {
            out.push(f(
                "capture-lane",
                format!("[kb.{name}] capture_dir"),
                "an explicit capture directory means this corpus receives captures; mount only public documents",
            ));
        }
        let p = kb.path.to_string_lossy();
        if p.starts_with("/home/")
            || p.starts_with("/Users/")
            || p == "/root"
            || p.starts_with("/root/")
        {
            out.push(f(
                "home-path",
                format!("[kb.{name}] path"),
                "the path is served verbatim in /api/kbs and doc responses; mount at a neutral path such as /srv/public-docs",
            ));
        }
    }

    if cfg.server.hostnames.is_empty() {
        out.push(f(
            "hostnames-unset",
            "[server] hostnames",
            "set the public host names so the Host guard rejects DNS-rebinding requests",
        ));
    }

    let suffix = cfg.server.artifact_host_suffix.to_ascii_lowercase();
    let parent_host = cfg
        .server
        .parent_origin
        .split("://")
        .last()
        .unwrap_or("")
        .split('/')
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if suffix.trim_start_matches('.').ends_with("localhost") || parent_host == "localhost" {
        out.push(f(
            "artifact-origin-default",
            "[server] parent_origin / artifact_host_suffix",
            "still the local-dev defaults; set parent_origin to the public SPA origin and artifact_host_suffix to the wildcard artifact domain",
        ));
    }

    let host = addr_host(&cfg.server.addr);
    if !host_is_private(host) {
        out.push(f(
            "bind-not-private",
            "[server] addr",
            format!(
                "`{}` is not loopback or a private address; bind loopback or a private network and publish only through the edge",
                cfg.server.addr
            ),
        ));
    }

    if cfg.sessions.live_transcripts_dir.is_some() {
        out.push(f(
            "sessions-live-dir",
            "[sessions] live_transcripts_dir",
            "live transcripts are unscrubbed session content; remove it from a mirror config",
        ));
    }

    if cfg.webhooks.as_ref().is_some_and(|w| !w.url.is_empty()) {
        out.push(f(
            "webhook-egress",
            "[webhooks]",
            "a webhook streams every daemon event (comments, sessions) off the box; remove it from a mirror config",
        ));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
[server]
addr = "127.0.0.1:4000"
parent_origin = "https://docs.example.com"
artifact_host_suffix = ".artifacts.example.com"
hostnames = ["docs.example.com"]

[kb.public-docs]
path = "/srv/public-docs"

[kb.public-docs.outbound]
strip_kb_prompt = true
"#;

    fn rules(toml: &str) -> Vec<&'static str> {
        let cfg = KbConfig::from_toml_str(toml).expect("parse");
        public_mirror_findings(&cfg)
            .iter()
            .map(|x| x.rule)
            .collect()
    }

    fn good_with(from: &str, to: &str) -> String {
        assert!(GOOD.contains(from), "fixture drift: {from}");
        GOOD.replace(from, to)
    }

    #[test]
    fn good_config_is_clean() {
        assert!(rules(GOOD).is_empty(), "{:?}", rules(GOOD));
    }

    #[test]
    fn no_corpus() {
        let t = "[server]\naddr=\"127.0.0.1:4000\"\nparent_origin=\"https://d.example.com\"\nartifact_host_suffix=\".artifacts.example.com\"\nhostnames=[\"d.example.com\"]\n";
        assert_eq!(rules(t), vec!["no-corpus"]);
    }

    #[test]
    fn strip_kb_prompt_missing_or_false() {
        let t = good_with("strip_kb_prompt = true", "strip_kb_prompt = false");
        assert_eq!(rules(&t), vec!["strip-kb-prompt"]);
        let t = good_with("[kb.public-docs.outbound]\nstrip_kb_prompt = true\n", "");
        assert_eq!(rules(&t), vec!["strip-kb-prompt"]);
    }

    #[test]
    fn hostnames_unset() {
        let t = good_with("hostnames = [\"docs.example.com\"]\n", "");
        assert_eq!(rules(&t), vec!["hostnames-unset"]);
    }

    #[test]
    fn artifact_origin_default() {
        let t = good_with("artifact_host_suffix = \".artifacts.example.com\"\n", "");
        assert_eq!(rules(&t), vec!["artifact-origin-default"]);
        let t = good_with("https://docs.example.com", "http://localhost:4000");
        assert_eq!(rules(&t), vec!["artifact-origin-default"]);
    }

    #[test]
    fn bind_not_private() {
        for bad in ["0.0.0.0:4000", "203.0.113.5:4000", "kb.example.com:4000"] {
            let t = good_with("127.0.0.1:4000", bad);
            assert_eq!(rules(&t), vec!["bind-not-private"], "{bad}");
        }
        for ok in [
            "[::1]:4000",
            "10.0.0.7:4000",
            "172.20.0.2:4000",
            "localhost:4000",
        ] {
            let t = good_with("127.0.0.1:4000", ok);
            assert!(rules(&t).is_empty(), "{ok}");
        }
    }

    #[test]
    fn private_corpus_by_name_category_and_scope() {
        let t = good_with("[kb.public-docs]", "[kb.memory-notes]")
            .replace("[kb.public-docs.outbound]", "[kb.memory-notes.outbound]");
        assert_eq!(rules(&t), vec!["private-corpus"]);
        let t = good_with(
            "path = \"/srv/public-docs\"",
            "path = \"/srv/public-docs\"\ndefault_search_category = \"memory-session\"",
        );
        assert_eq!(rules(&t), vec!["private-corpus"]);
        let t = good_with(
            "path = \"/srv/public-docs\"",
            "path = \"/srv/public-docs\"\nmemory_scope = \"x\"",
        );
        assert_eq!(rules(&t), vec!["private-corpus"]);
    }

    #[test]
    fn capture_lane() {
        let t = good_with(
            "path = \"/srv/public-docs\"",
            "path = \"/srv/public-docs\"\ncapture_dir = \"capture\"",
        );
        assert_eq!(rules(&t), vec!["capture-lane"]);
    }

    #[test]
    fn home_path() {
        let t = good_with("/srv/public-docs", "/home/someone/docs");
        assert_eq!(rules(&t), vec!["home-path"]);
        for bad in ["/root", "/root/docs", "/Users/someone/docs"] {
            let t = good_with("/srv/public-docs", bad);
            assert_eq!(rules(&t), vec!["home-path"], "{bad}");
        }
        // `/root` is a directory, not a string prefix.
        for ok in ["/rootfs/docs", "/rootless-docs", "/srv/root/docs"] {
            let t = good_with("/srv/public-docs", ok);
            assert!(rules(&t).is_empty(), "{ok}");
        }
    }

    #[test]
    fn sessions_live_dir() {
        let t = format!("{GOOD}\n[sessions]\nlive_transcripts_dir = \"/srv/t\"\n");
        assert_eq!(rules(&t), vec!["sessions-live-dir"]);
    }

    #[test]
    fn webhook_egress() {
        let t = format!(
            "{GOOD}\n[webhooks]\nurl = \"https://hooks.example.com/x\"\ntypes = [\"artifact.indexed\"]\n"
        );
        assert_eq!(rules(&t), vec!["webhook-egress"]);
    }

    #[test]
    fn every_rule_id_is_listed_in_rules() {
        let bad = r#"
[server]
addr = "0.0.0.0:4000"
[sessions]
live_transcripts_dir = "/srv/t"
[webhooks]
url = "https://hooks.example.com/x"
types = ["artifact.indexed"]
[kb.memory-x]
path = "/home/a/b"
capture_dir = "capture"
"#;
        let got = rules(bad);
        for r in &got {
            assert!(RULES.contains(r), "{r} missing from RULES");
        }
        for r in RULES.iter().filter(|r| **r != "no-corpus") {
            assert!(got.contains(r), "{r} not exercised");
        }
    }
}
