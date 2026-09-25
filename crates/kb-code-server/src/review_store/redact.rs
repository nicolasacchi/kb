//! RS-U2 — the redactor every captured git/gh stderr, stdout-for-logging
//! and error detail passes through before it can reach a log line, an
//! error envelope, or the DB (design-internal-store §4.4/§5.5/§13).
//!
//! Two layers, both always applied:
//!
//! 1. **Shape rules** — anything that LOOKS like a secret, whoever minted
//!    it: GitHub tokens (`ghp_`/`gho_`/`ghs_`/`ghu_`/`ghr_`,
//!    `github_pat_`), GitLab PATs (`glpat-`), URL userinfo
//!    (`scheme://user:pass@host` → `scheme://[redacted]@host`),
//!    `Authorization:` / `Proxy-Authorization:` header values, `Bearer`/
//!    `Basic`/`token` credentials, and a credential-protocol
//!    `password=` line.
//! 2. **Known literals** — the exact secret(s) the calling operation had in
//!    hand ([`redact_with`]), so a token with a shape rule 1 does not know
//!    (a GHE or Gitea token) is still stripped.
//!
//! Over-redaction is the accepted failure mode: a 40-hex object id is NOT
//! matched (it would destroy every useful git error), but a long opaque
//! word after `Bearer ` is.

use std::borrow::Cow;
use std::sync::LazyLock;

use super::cred::MIN_SECRET_LEN;
use regex::Regex;

/// What every redacted span becomes.
pub const REDACTED: &str = "[redacted]";

struct Rule {
    re: Regex,
    replace: &'static str,
}

static RULES: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    let r = |re: &str, replace: &'static str| Rule {
        re: Regex::new(re).expect("static redaction regex"),
        replace,
    };
    vec![
        // Header values run to end of line — an Authorization header's
        // value is secret whatever its scheme.
        r(
            r"(?i)\b((?:proxy-)?authorization)\s*:[^\r\n]*",
            "${1}: [redacted]",
        ),
        // URL userinfo: `https://x-access-token:ghp_…@github.com/…`.
        r(
            r"(?i)\b([a-z][a-z0-9+.\-]*://)[^/@\s'`]+@",
            "${1}[redacted]@",
        ),
        r(r"\bgh[pousr]_[A-Za-z0-9]{16,}", REDACTED),
        r(r"\bgithub_pat_[A-Za-z0-9_]{20,}", REDACTED),
        r(r"\bglpat-[A-Za-z0-9_\-]{16,}", REDACTED),
        r(
            r"(?i)\b(bearer|basic|token)(\s+)[A-Za-z0-9._~+/=\-]{16,}",
            "${1}${2}[redacted]",
        ),
        // git-credential protocol line.
        r(r"(?im)^(\s*password\s*=).*$", "${1}[redacted]"),
    ]
});

/// Redact the shape rules only.
pub fn redact(s: &str) -> String {
    redact_with(s, &[])
}

/// Redact the shape rules AND every literal in `secrets` (empty and
/// very short literals are ignored — redacting every `a` would make the
/// output useless without protecting anything).
pub fn redact_with(s: &str, secrets: &[&str]) -> String {
    let mut out: Cow<'_, str> = Cow::Borrowed(s);
    for lit in secrets.iter().filter(|l| l.len() >= MIN_SECRET_LEN) {
        if out.contains(lit) {
            out = Cow::Owned(out.replace(lit, REDACTED));
        }
    }
    for rule in RULES.iter() {
        let next = match rule.re.replace_all(&out, rule.replace) {
            Cow::Owned(o) => Some(o),
            Cow::Borrowed(_) => None,
        };
        if let Some(o) = next {
            out = Cow::Owned(o);
        }
    }
    out.into_owned()
}

/// Lossy-decode, redact, and cap at `max` bytes (on a char boundary) —
/// the one path from raw subprocess bytes to a storable/loggable string.
pub fn redact_bytes(bytes: &[u8], secrets: &[&str], max: usize) -> String {
    let s = String::from_utf8_lossy(bytes);
    let mut red = redact_with(&s, secrets);
    if red.len() > max {
        let mut cut = max;
        while !red.is_char_boundary(cut) {
            cut -= 1;
        }
        red.truncate(cut);
        red.push_str("…[truncated]");
    }
    red
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_token_shapes_are_redacted() {
        for t in [
            "ghp_FAKEaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "gho_FAKEbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "ghs_FAKEcccccccccccccccccccccccccccc",
            "ghu_FAKEdddddddddddddddddddddddddddd",
            "ghr_FAKEeeeeeeeeeeeeeeeeeeeeeeeeeeee",
            "github_pat_11FAKE0000000000_ffffffffffffffffffffffffffffffffffff",
            "glpat-FAKEgggggggggggggggg",
        ] {
            let out = redact(&format!("error: token {t} was rejected"));
            assert!(!out.contains(t), "{t} leaked: {out}");
            assert!(out.contains(REDACTED), "{out}");
        }
    }

    #[test]
    fn url_userinfo_is_redacted_but_host_and_path_survive() {
        let out = redact(
            "fatal: unable to access 'https://x-access-token:ghp_FAKEzzzzzzzzzzzzzzzzzzzzz@github.com/acme/widgets.git/': 403",
        );
        assert!(!out.contains("x-access-token"), "{out}");
        assert!(!out.contains("ghp_"), "{out}");
        assert!(
            out.contains("https://[redacted]@github.com/acme/widgets.git/"),
            "{out}"
        );
        // A user-only form too.
        let out = redact("remote https://someone@example.com/r.git");
        assert!(
            out.contains("https://[redacted]@example.com/r.git"),
            "{out}"
        );
    }

    #[test]
    fn authorization_headers_and_schemes_are_redacted() {
        let out = redact("> Authorization: Basic eC1hY2Nlc3MtdG9rZW46c2VjcmV0\n> Host: x");
        assert_eq!(out, "> Authorization: [redacted]\n> Host: x");
        let out = redact("proxy-authorization: Bearer abc");
        assert_eq!(out, "proxy-authorization: [redacted]");
        let out = redact("using Bearer AAAABBBBCCCCDDDDEEEE now");
        assert_eq!(out, "using Bearer [redacted] now");
    }

    #[test]
    fn credential_protocol_password_lines_are_redacted() {
        let out = redact("protocol=https\nhost=github.com\nusername=x\npassword=hunter2hunter2\n");
        assert!(!out.contains("hunter2"), "{out}");
        assert!(out.contains("password=[redacted]"), "{out}");
        assert!(out.contains("host=github.com"));
    }

    #[test]
    fn known_literals_are_redacted_even_without_a_known_shape() {
        let out = redact_with(
            "gitea said: 0123456789abcdefXYZ is bad",
            &["0123456789abcdefXYZ"],
        );
        assert_eq!(out, "gitea said: [redacted] is bad");
        // short literals are ignored rather than shredding the output
        assert_eq!(redact_with("a b c", &["a"]), "a b c");
    }

    #[test]
    fn object_ids_and_ordinary_errors_survive() {
        let s = "fatal: remote error: upload-pack: not our ref deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
        assert_eq!(redact(s), s);
        let s = "fatal: couldn't find remote ref refs/heads/main";
        assert_eq!(redact(s), s);
    }

    #[test]
    fn redact_bytes_caps_on_a_char_boundary() {
        let out = redact_bytes("ééééé".as_bytes(), &[], 3);
        assert!(out.starts_with('é'));
        assert!(out.ends_with("[truncated]"));
    }
}
