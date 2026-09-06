//! `kb-core::session_scrub` — deterministic, LLM-free redaction of a session
//! transcript before it leaves the account (Session Portability, SP2).
//!
//! A raw session transcript is the whole conversation: tool-output dumps,
//! pasted keys, env vars, absolute `$HOME` paths. Exporting one to ANOTHER
//! account is deliberate data egress, so `kb sessions export --scrub` runs this
//! pass first. It is **configurable + composable** ([`ScrubOptions`]): three
//! independent layers you opt into per export —
//!
//!   - `secrets` — high-precision known token shapes (`sk-…`, `ghp_…`, AWS,
//!     Slack, Google, JWT, PEM private keys, `Bearer …`), labeled key/value
//!     secrets (`"api_key": "…"`, `password=…`), and secret-named env vars.
//!     The safe floor — near-zero false positives.
//!   - `paths` — anonymise `/home/<user>` and `/Users/<user>` usernames
//!     (rewrites historical paths; `claude -r` uses the *current* cwd, so this
//!     is cosmetic for resume).
//!   - `entropy` — a Shannon-entropy sweep for long unlabeled random-looking
//!     blobs. Catches more, but can over-redact real base64/hashes; opt-in.
//!
//! Everything is regex/entropy only (no model, no RNG) so the same input always
//! yields the same output + [`ScrubReport`]. It rewrites secret SUBSTRINGS in
//! place (the redaction markers are ASCII + never contain `"`), so the JSONL
//! line structure stays valid and `claude -r` still parses each record.

use regex::{Captures, Regex};
use std::collections::BTreeMap;
use std::sync::OnceLock;

/// Which redaction layers to apply. All-off is a no-op.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScrubOptions {
    /// Known token shapes + labeled secrets + secret-named env vars.
    pub secrets: bool,
    /// `/home/<user>` and `/Users/<user>` username anonymisation.
    pub paths: bool,
    /// High-entropy sweep for long unlabeled blobs.
    pub entropy: bool,
}

impl ScrubOptions {
    /// The safe floor: secrets only.
    pub fn secrets_only() -> Self {
        Self {
            secrets: true,
            paths: false,
            entropy: false,
        }
    }
    /// True when at least one layer is on (i.e. scrubbing was requested).
    pub fn any(&self) -> bool {
        self.secrets || self.paths || self.entropy
    }
}

/// What a scrub pass removed — a total plus per-kind counts, for the preview +
/// the manifest's `redactions_applied`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScrubReport {
    pub total: u32,
    pub by_kind: BTreeMap<String, u32>,
}

impl ScrubReport {
    fn bump(&mut self, kind: &str, n: u32) {
        if n == 0 {
            return;
        }
        self.total += n;
        *self.by_kind.entry(kind.to_string()).or_default() += n;
    }
}

/// Entropy (bits/char) above which the opt-in `entropy` layer redacts a long
/// candidate run. Chosen so pure-hex (≤4.0 — git SHAs, md5/sha hex) is spared
/// but high-alphabet random base64/secret material (~5–6) is caught.
const ENTROPY_THRESHOLD: f64 = 4.2;
/// Minimum length for an entropy candidate (short strings can't be confidently
/// classified + are rarely secrets on their own).
const ENTROPY_MIN_LEN: usize = 32;

/// Redact a raw JSONL transcript per `opts`. Returns the redacted text + a
/// report. Deterministic; a no-op (clone + empty report) when no layer is on.
pub fn scrub_transcript(jsonl: &str, opts: &ScrubOptions) -> (String, ScrubReport) {
    let mut text = jsonl.to_string();
    let mut report = ScrubReport::default();
    if opts.secrets {
        apply_token_rules(&mut text, &mut report);
        apply_group_rules(&mut text, labeled_rules(), &mut report);
    }
    if opts.paths {
        apply_group_rules(&mut text, path_rules(), &mut report);
    }
    if opts.entropy {
        apply_entropy(&mut text, &mut report);
    }
    (text, report)
}

// --- rules -------------------------------------------------------------------

/// A rule whose ENTIRE match is a secret → replaced with `[redacted:kind]`.
struct TokenRule {
    kind: &'static str,
    re: Regex,
}

/// A rule with two groups `(keep)(secret)` — group 1 is preserved, group 2 is
/// swapped for `mask` (so the label/prefix stays legible).
struct GroupRule {
    kind: &'static str,
    re: Regex,
    mask: &'static str,
}

fn token_rules() -> &'static [TokenRule] {
    static RULES: OnceLock<Vec<TokenRule>> = OnceLock::new();
    RULES.get_or_init(|| {
        let mk = |kind, pat: &str| TokenRule {
            kind,
            re: Regex::new(pat).expect("valid token regex"),
        };
        // Order: widest/most-specific first so a broad rule can't shadow a
        // precise one. PEM spans (on one physical line, `\n` escaped) first.
        vec![
            mk(
                "private-key",
                r"-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----",
            ),
            mk("github-pat", r"github_pat_[A-Za-z0-9_]{60,}"),
            mk("github-token", r"gh[pousr]_[A-Za-z0-9]{36,}"),
            mk("aws-access-key-id", r"AKIA[0-9A-Z]{16}"),
            mk("slack-token", r"xox[baprs]-[A-Za-z0-9-]{10,}"),
            mk("google-api-key", r"AIza[0-9A-Za-z_\-]{35}"),
            mk("api-key", r"sk-[A-Za-z0-9_\-]{20,}"),
            mk(
                "jwt",
                r"eyJ[A-Za-z0-9_\-]{6,}\.[A-Za-z0-9_\-]{6,}\.[A-Za-z0-9_\-]{6,}",
            ),
        ]
    })
}

fn labeled_rules() -> &'static [GroupRule] {
    static RULES: OnceLock<Vec<GroupRule>> = OnceLock::new();
    RULES.get_or_init(|| {
        let mk = |kind, pat: &str, mask| GroupRule {
            kind,
            re: Regex::new(pat).expect("valid labeled regex"),
            mask,
        };
        vec![
            // `"api_key": "VALUE"`  /  `password=VALUE`  /  `secret: VALUE`
            mk(
                "labeled-secret",
                r#"(?i)("?(?:api[_-]?key|apikey|secret|token|password|passwd|access[_-]?token|refresh[_-]?token|auth[_-]?token|client[_-]?secret|private[_-]?key)"?\s*[:=]\s*"?)([A-Za-z0-9+/=_\-\.]{8,})"#,
                "[redacted:labeled-secret]",
            ),
            // `Authorization: Bearer TOKEN`
            mk(
                "bearer-token",
                r"(?i)(bearer\s+)([A-Za-z0-9._\-]{16,})",
                "[redacted:bearer-token]",
            ),
            // `AWS_SECRET_ACCESS_KEY=VALUE` — env var whose NAME implies a secret.
            mk(
                "env-secret",
                r"\b([A-Z][A-Z0-9_]*(?:KEY|TOKEN|SECRET|PASSWORD|PASSWD|CREDENTIAL|CREDENTIALS|APIKEY)[A-Z0-9_]*=)([^\s\x22\\]+)",
                "[masked]",
            ),
        ]
    })
}

fn path_rules() -> &'static [GroupRule] {
    static RULES: OnceLock<Vec<GroupRule>> = OnceLock::new();
    RULES.get_or_init(|| {
        let mk = |pat: &str| GroupRule {
            kind: "home-path",
            re: Regex::new(pat).expect("valid path regex"),
            mask: "[user]",
        };
        vec![
            mk(r"(/home/)([A-Za-z0-9_][A-Za-z0-9_.\-]*)"),
            mk(r"(/Users/)([A-Za-z0-9_][A-Za-z0-9_.\-]*)"),
        ]
    })
}

fn apply_token_rules(text: &mut String, report: &mut ScrubReport) {
    for rule in token_rules() {
        let mut n = 0u32;
        let replaced = rule
            .re
            .replace_all(text.as_str(), |_: &Captures| {
                n += 1;
                format!("[redacted:{}]", rule.kind)
            })
            .into_owned();
        *text = replaced;
        report.bump(rule.kind, n);
    }
}

fn apply_group_rules(text: &mut String, rules: &[GroupRule], report: &mut ScrubReport) {
    for rule in rules {
        let mut n = 0u32;
        let replaced = rule
            .re
            .replace_all(text.as_str(), |caps: &Captures| {
                n += 1;
                format!("{}{}", &caps[1], rule.mask)
            })
            .into_owned();
        *text = replaced;
        report.bump(rule.kind, n);
    }
}

fn apply_entropy(text: &mut String, report: &mut ScrubReport) {
    static RE: OnceLock<Regex> = OnceLock::new();
    // GC-B8 — `/` is deliberately EXCLUDED from the run char class. A JSONL
    // transcript line escapes a literal `/` as `\/`; if a high-entropy run
    // were allowed to start ON that `/`, the match (and its replacement)
    // consumes the slash but leaves the preceding backslash behind, turning
    // a valid `\/` escape into an invalid `\[` one (the redaction marker
    // starts with `[`) and corrupting the line's JSON. Dropping `/` from the
    // class means a match can never begin (or continue) across a `/`, so
    // whatever precedes a match — including a `\/` escape — is always left
    // intact. This still catches real base64 secrets: `/` only ever splits
    // one candidate run into two (the alphanumeric halves either side of
    // it), each of which is still redacted on its own once it clears
    // `ENTROPY_MIN_LEN` — coverage of the secret's actual entropy is
    // unaffected, only the single low-information separator survives.
    let re = RE.get_or_init(|| {
        Regex::new(&format!(r"[A-Za-z0-9+=_\-]{{{ENTROPY_MIN_LEN},}}"))
            .expect("valid entropy regex")
    });
    let mut n = 0u32;
    let replaced = re
        .replace_all(text.as_str(), |caps: &Captures| {
            let m = &caps[0];
            // Don't re-redact a marker we just wrote.
            if m.contains("redacted") || shannon_entropy(m) <= ENTROPY_THRESHOLD {
                m.to_string()
            } else {
                n += 1;
                "[redacted:high-entropy]".to_string()
            }
        })
        .into_owned();
    *text = replaced;
    report.bump("high-entropy", n);
}

/// Shannon entropy of `s` in bits per byte.
fn shannon_entropy(s: &str) -> f64 {
    let mut counts = [0u32; 256];
    let mut total = 0u32;
    for b in s.bytes() {
        counts[b as usize] += 1;
        total += 1;
    }
    if total == 0 {
        return 0.0;
    }
    let total = total as f64;
    counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / total;
            -p * p.log2()
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secrets() -> ScrubOptions {
        ScrubOptions::secrets_only()
    }

    #[test]
    fn redacts_known_token_shapes() {
        let cases = [
            (
                "call with sk-ant-abcdefghijklmnopqrstuvwxyz012345",
                "api-key",
            ),
            (
                "token ghp_0123456789abcdefghijklmnopqrstuvwxyzAB",
                "github-token",
            ),
            ("id AKIAIOSFODNN7EXAMPLE here", "aws-access-key-id"),
            (
                "goog AIzaSyA1234567890abcdefghijklmnopqrstuvw done",
                "google-api-key",
            ),
        ];
        for (input, kind) in cases {
            let (out, rep) = scrub_transcript(input, &secrets());
            assert!(out.contains(&format!("[redacted:{kind}]")), "{kind}: {out}");
            assert_eq!(rep.by_kind.get(kind), Some(&1), "{kind} counted once");
        }
    }

    #[test]
    fn redacts_pem_private_key_on_one_line() {
        let jsonl = r#"{"text":"key -----BEGIN RSA PRIVATE KEY-----\nMIIabc123\n-----END RSA PRIVATE KEY----- ok"}"#;
        let (out, rep) = scrub_transcript(jsonl, &secrets());
        assert!(out.contains("[redacted:private-key]"), "{out}");
        assert!(!out.contains("MIIabc123"));
        assert_eq!(rep.by_kind.get("private-key"), Some(&1));
        // JSON structure preserved.
        assert!(out.starts_with(r#"{"text":"#) && out.ends_with("}"));
    }

    #[test]
    fn redacts_labeled_and_env_secrets_keeps_label() {
        let jsonl =
            r#"{"a":"api_key: AbCdEf01234567","b":"AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMIabcdefgh"}"#;
        let (out, rep) = scrub_transcript(jsonl, &secrets());
        assert!(out.contains("api_key: [redacted:labeled-secret]"), "{out}");
        assert!(out.contains("AWS_SECRET_ACCESS_KEY=[masked]"), "{out}");
        assert!(!out.contains("AbCdEf01234567"));
        assert!(!out.contains("wJalrXUtnFEMIabcdefgh"));
        assert_eq!(rep.by_kind.get("labeled-secret"), Some(&1));
        assert_eq!(rep.by_kind.get("env-secret"), Some(&1));
    }

    #[test]
    fn paths_layer_is_independent_of_secrets() {
        let jsonl = r#"{"cwd":"/home/user/project/kb","u":"/Users/Bob/x"}"#;
        // secrets-only leaves paths intact.
        let (out, _) = scrub_transcript(jsonl, &secrets());
        assert!(
            out.contains("/home/user/project/kb"),
            "secrets-only keeps paths"
        );
        // paths layer anonymises the username segment, preserving the rest.
        let opts = ScrubOptions {
            secrets: false,
            paths: true,
            entropy: false,
        };
        let (out, rep) = scrub_transcript(jsonl, &opts);
        assert!(out.contains("/home/[user]/project/kb"), "{out}");
        assert!(out.contains("/Users/[user]/x"), "{out}");
        assert_eq!(rep.by_kind.get("home-path"), Some(&2));
    }

    #[test]
    fn entropy_layer_opt_in_spares_hex_catches_random() {
        let sha = "a".repeat(40); // low-entropy (all same char) → spared
        let hexish = "0123456789abcdef0123456789abcdef01234567"; // 40 hex ≈ 4.0 → spared
        let secret = "Xq7Zp2Lm9Wk4Rt6Yv8Bn3Cs5Df1Gh0Jl2Nm4Pr6Tw8Zx"; // high-entropy
        let jsonl = format!(r#"{{"s":"{sha}","h":"{hexish}","k":"{secret}"}}"#);
        // With entropy OFF, nothing touched.
        let (out, rep) = scrub_transcript(&jsonl, &secrets());
        assert_eq!(rep.total, 0, "entropy off → no redactions");
        assert!(out.contains(secret));
        // With entropy ON, only the high-entropy blob goes.
        let opts = ScrubOptions {
            secrets: false,
            paths: false,
            entropy: true,
        };
        let (out, rep) = scrub_transcript(&jsonl, &opts);
        assert!(out.contains(&sha), "all-same-char spared");
        assert!(out.contains(hexish), "pure hex spared");
        assert!(!out.contains(secret), "high-entropy redacted");
        assert_eq!(rep.by_kind.get("high-entropy"), Some(&1));
    }

    /// GC-B8 — a high-entropy run starting immediately after a JSON-escaped
    /// `\/` must not consume the `/` (which would leave the preceding `\`
    /// dangling in front of the `[redacted:…]` marker and produce an invalid
    /// `\[` escape). Exact shape: `"url":"http:\/\/<high-entropy>"`.
    #[test]
    fn entropy_layer_does_not_corrupt_escaped_forward_slash() {
        let secret = "Xq7Zp2Lm9Wk4Rt6Yv8Bn3Cs5Df1Gh0Jl2Nm4Pr6Tw8Zx"; // high-entropy
        let jsonl = format!(r#"{{"url":"http:\/\/{secret}"}}"#);
        let opts = ScrubOptions {
            secrets: false,
            paths: false,
            entropy: true,
        };
        let (out, rep) = scrub_transcript(&jsonl, &opts);
        assert!(
            !out.contains(secret),
            "the secret itself is redacted: {out}"
        );
        // The escaped slashes must survive untouched — no dangling `\[`.
        assert!(out.contains(r#"http:\/\/[redacted:high-entropy]"#), "{out}");
        assert!(
            !out.contains(r"\["),
            "no dangling backslash before the marker: {out}"
        );
        assert_eq!(rep.by_kind.get("high-entropy"), Some(&1));
        // The line must still parse as valid JSON.
        serde_json::from_str::<serde_json::Value>(&out)
            .unwrap_or_else(|e| panic!("scrubbed line not valid JSON: {e}\n{out}"));
    }

    #[test]
    fn deterministic_and_no_op_when_all_off() {
        let jsonl = r#"{"api_key":"sk-abcdefghijklmnopqrstuvwx","p":"/home/user/x"}"#;
        let opts = ScrubOptions {
            secrets: true,
            paths: true,
            entropy: true,
        };
        let a = scrub_transcript(jsonl, &opts);
        let b = scrub_transcript(jsonl, &opts);
        assert_eq!(a.0, b.0, "same input → same output");
        assert_eq!(a.1, b.1, "same input → same report");
        // No layer on → verbatim passthrough, empty report.
        let (out, rep) = scrub_transcript(jsonl, &ScrubOptions::default());
        assert_eq!(out, jsonl);
        assert_eq!(rep.total, 0);
    }

    #[test]
    fn scrubbed_output_stays_valid_json_per_line() {
        let jsonl = r#"{"sessionId":"abc","text":"token sk-abcdefghijklmnopqrstuvwx and AWS_SECRET_KEY=hunter2xxxxxxxx"}"#;
        let opts = ScrubOptions::secrets_only();
        let (out, _) = scrub_transcript(jsonl, &opts);
        // Each line still parses as JSON (the markers contain no `"`).
        for line in out.lines() {
            serde_json::from_str::<serde_json::Value>(line)
                .unwrap_or_else(|e| panic!("scrubbed line not valid JSON: {e}\n{line}"));
        }
    }
}
