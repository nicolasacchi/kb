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
    let mut report = ScrubReport::default();
    // Secrets-only fast path: the layers are applied SEQUENTIALLY, so when no
    // rule matches the ORIGINAL text, rule 1 is a no-op, rule 2 sees the same
    // text, and so on — the output is the input, byte for byte. One
    // multi-pattern scan then replaces ~12 per-rule scans + copies on the
    // (overwhelmingly common) clean chunk.
    if opts.secrets && !opts.paths && !opts.entropy && !secrets_prefilter().is_match(jsonl) {
        return (jsonl.to_string(), report);
    }
    let mut text = jsonl.to_string();
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
            mk("authelia-token", r"authelia_at_[A-Za-z0-9_\-.]{20,}"),
            mk(
                "jwt",
                r"eyJ[A-Za-z0-9_\-]{6,}\.[A-Za-z0-9_\-]{6,}\.[A-Za-z0-9_\-]{6,}",
            ),
        ]
    })
}

/// The kind of the first HIGH-PRECISION credential token in `s`, or `None`.
/// Built on the same `secrets` table the transcript scrub redacts with. The
/// lint predicts the dispatcher's secret gate, so it must never be LESS
/// sensitive than it: the GitHub-family bodies (classic and fine-grained PAT)
/// floor at 20 — the dispatcher's own scan floor — instead of the scrub's
/// stricter 36/60, and `sk-` has NO word-boundary requirement (the dispatcher
/// scans `sk-[A-Za-z0-9]{20,}` anywhere, so `desk-` followed by a 20-char
/// ALPHANUMERIC run is flagged by the gate and therefore here). The `sk-`
/// body class is alphanumeric only (no `_`/`-`), exactly the dispatcher's, so
/// a hyphenated slug such as `task-based-error-handling-rules` (whose `sk-`
/// is followed by short words) is NOT flagged. Short prose such as
/// `desk-shell`, or a bare prefix (a mention of `ghp_`), never matches: every
/// rule requires a credential-length body.
pub fn first_secret_token_kind(s: &str) -> Option<&'static str> {
    const OVERRIDDEN: [&str; 3] = ["github-token", "github-pat", "api-key"];
    static LINT: OnceLock<[TokenRule; 3]> = OnceLock::new();
    let lint = LINT.get_or_init(|| {
        let mk = |kind, pat: &str| TokenRule {
            kind,
            re: Regex::new(pat).expect("valid regex"),
        };
        [
            mk("github-token", r"gh[pousr]_[A-Za-z0-9]{20,}"),
            mk("github-pat", r"github_pat_[A-Za-z0-9_]{20,}"),
            mk("api-key", r"sk-[A-Za-z0-9]{20,}"),
        ]
    });
    if let Some(r) = lint.iter().find(|r| r.re.is_match(s)) {
        return Some(r.kind);
    }
    token_rules()
        .iter()
        .filter(|r| !OVERRIDDEN.contains(&r.kind))
        .find(|r| r.re.is_match(s))
        .map(|r| r.kind)
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

/// One `RegexSet` over every `secrets`-layer pattern (token + labeled rules),
/// used only as a no-match fast path by [`scrub_transcript`].
fn secrets_prefilter() -> &'static regex::RegexSet {
    static SET: OnceLock<regex::RegexSet> = OnceLock::new();
    SET.get_or_init(|| {
        let pats: Vec<&str> = token_rules()
            .iter()
            .map(|r| r.re.as_str())
            .chain(labeled_rules().iter().map(|r| r.re.as_str()))
            .collect();
        regex::RegexSet::new(pats).expect("valid secrets prefilter set")
    })
}

fn apply_token_rules(text: &mut String, report: &mut ScrubReport) {
    for rule in token_rules() {
        let mut n = 0u32;
        let owned = match rule.re.replace_all(text.as_str(), |_: &Captures| {
            n += 1;
            format!("[redacted:{}]", rule.kind)
        }) {
            std::borrow::Cow::Owned(s) => Some(s),
            std::borrow::Cow::Borrowed(_) => None,
        };
        if let Some(s) = owned {
            *text = s;
        }
        report.bump(rule.kind, n);
    }
}

fn apply_group_rules(text: &mut String, rules: &[GroupRule], report: &mut ScrubReport) {
    for rule in rules {
        let mut n = 0u32;
        let owned = match rule.re.replace_all(text.as_str(), |caps: &Captures| {
            n += 1;
            format!("{}{}", &caps[1], rule.mask)
        }) {
            std::borrow::Cow::Owned(s) => Some(s),
            std::borrow::Cow::Borrowed(_) => None,
        };
        if let Some(s) = owned {
            *text = s;
        }
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

/// Minimum size at which [`SecretScrubStream`] considers cutting a new chunk.
pub const SCRUB_STREAM_CHUNK_BYTES: usize = 1024 * 1024;

/// Incremental, chunk-local twin of [`scrub_transcript`] with
/// [`ScrubOptions::secrets_only`], for callers that must not hold a whole
/// multi-MB transcript in memory. Lines are fed in order; the stream buffers
/// them and only scrubs a chunk when it may cut cleanly.
///
/// **Cut rule (what makes chunked output byte-identical to a whole-text
/// scrub).** A cut is taken only between a `\n` and a following line whose
/// FIRST byte is `{` (every JSONL record). No `secrets` pattern can match
/// across such a cut: the only constructs able to consume a newline are the
/// `\s*`/`\s+` runs of the labeled and bearer rules, and each needs a further
/// `[:=]` or token character AFTER the whitespace, while the byte right after
/// the cut is `{` (not whitespace, not `:`/`=`, not in any value class); every
/// other rule (`.*?`, `[^\s…]+`, character classes) cannot contain `\n`. The
/// longest span any single match can have is therefore bounded by the line it
/// sits on, so there is no cut-straddling partial secret by construction —
/// the cut never lands inside a match. Text that never offers such a cut
/// (non-JSONL, one giant line) simply stays one chunk: correctness first,
/// memory second.
pub struct SecretScrubStream {
    pending: String,
    /// Minimum pending size before a cut is considered (tests shrink it).
    chunk_bytes: usize,
    /// Total redactions across every flushed chunk.
    pub redactions: u32,
}

impl Default for SecretScrubStream {
    fn default() -> Self {
        Self::new()
    }
}

impl SecretScrubStream {
    pub fn new() -> Self {
        Self {
            pending: String::new(),
            chunk_bytes: SCRUB_STREAM_CHUNK_BYTES,
            redactions: 0,
        }
    }

    /// Test-only: cut at EVERY allowed boundary at/after `chunk_bytes`.
    #[cfg(test)]
    fn with_chunk_bytes(chunk_bytes: usize) -> Self {
        Self {
            chunk_bytes,
            ..Self::new()
        }
    }

    fn flush(&mut self, sink: &mut impl FnMut(&str)) {
        if self.pending.is_empty() {
            return;
        }
        let (out, report) = scrub_transcript(&self.pending, &ScrubOptions::secrets_only());
        self.redactions += report.total;
        sink(&out);
        self.pending.clear();
    }

    /// Feed the next physical line (INCLUDING its terminating `\n`, except
    /// possibly the last line of the input). Scrubbed chunks are handed to
    /// `sink` in order.
    pub fn push_line(&mut self, line: &str, sink: &mut impl FnMut(&str)) {
        if self.pending.len() >= self.chunk_bytes
            && self.pending.ends_with('\n')
            && line.starts_with('{')
        {
            self.flush(sink);
        }
        self.pending.push_str(line);
    }

    /// Flush the final chunk.
    pub fn finish(&mut self, sink: &mut impl FnMut(&str)) {
        self.flush(sink);
    }
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

    #[test]
    fn lint_secret_kind_matches_dispatcher_shapes_and_not_prose() {
        // Golden: the dispatcher scans `sk-[A-Za-z0-9]{20,}` / `ghp_[A-Za-z0-9]{20,}`.
        assert_eq!(
            first_secret_token_kind("sk-abcdefghij0123456789"),
            Some("api-key")
        );
        assert_eq!(
            first_secret_token_kind("ghp_abcdefghij0123456789"),
            Some("github-token")
        );
        assert_eq!(
            first_secret_token_kind(&format!("{}{}", "AKIA", "ABCDEFGHIJKLMNOP")),
            Some("aws-access-key-id")
        );
        // The dispatcher scans `sk-` anywhere: a >=20-char ALPHANUMERIC run
        // after `desk-` is flagged by the gate, so the lint must flag it too.
        assert_eq!(
            first_secret_token_kind("desk-abcdefghij0123456789"),
            Some("api-key")
        );
        // Hyphenated slugs are prose, not credentials (the body class has no `-`).
        for slug in [
            "disk-bound-and-everything-else-too",
            "task-based-error-handling-rules",
        ] {
            assert_eq!(first_secret_token_kind(slug), None, "{slug}");
        }
        assert_eq!(
            first_secret_token_kind("x-sk-abcdefghij0123456789"),
            Some("api-key")
        );
        // Floors: short fine-grained PAT bodies and authelia tokens are covered.
        assert_eq!(
            first_secret_token_kind("github_pat_11ABCDEFG0abcdefghij_xyz"),
            Some("github-pat")
        );
        assert_eq!(
            first_secret_token_kind("authelia_at_abcdefghij0123456789ABCDEF"),
            Some("authelia-token")
        );
        for prose in ["desk-shell", "disk-bound-and-so-on", "ghp_ in prose", "sk-"] {
            assert_eq!(first_secret_token_kind(prose), None, "{prose}");
        }
    }

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
    fn scrub_redacts_authelia_access_tokens() {
        let (out, rep) = scrub_transcript(
            "bearer-less leak authelia_at_abcdefghij0123456789ABCDEFGHIJ-_. end",
            &secrets(),
        );
        assert!(out.contains("[redacted:authelia-token]"), "{out}");
        assert!(!out.contains("abcdefghij0123456789"), "{out}");
        assert_eq!(rep.by_kind.get("authelia-token"), Some(&1));
        let (prose, _) = scrub_transcript("the authelia_at_ prefix is documented", &secrets());
        assert!(prose.contains("authelia_at_ prefix"), "{prose}");
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

    /// The chunked stream is byte-identical to a whole-text scrub — including
    /// newline-spanning `\s+` matches (bearer, labeled) that must NOT be cut
    /// even when a chunk boundary is "due" — and reports the same total.
    #[test]
    fn stream_chunking_is_byte_identical_to_whole_text_scrub() {
        let mut text = String::new();
        let mut i = 0usize;
        while text.len() < 3 * SCRUB_STREAM_CHUNK_BYTES + 5_000 {
            text.push_str(&format!(
                "{{\"i\":{i},\"pad\":\"lorem ipsum dolor sit amet\"}}\n"
            ));
            if i % 400 == 0 {
                text.push_str("Authorization: Bearer\nabcdefghijklmnopqrstuvwxyz0123456789\n");
                text.push_str("\"password\"\n:  hunter2hunter2\n");
                text.push_str(
                    "{\"k\":\"sk-ABCDEFGHIJKLMNOPQRSTUVWXYZ\",\"e\":\"AWS_SECRET_KEY=abc12345\"}\n",
                );
            }
            i += 1;
        }
        let (whole, report) = scrub_transcript(&text, &ScrubOptions::secrets_only());
        let mut stream = SecretScrubStream::new();
        let mut out = String::new();
        let mut chunks = 0;
        let mut sink = |c: &str| {
            chunks += 1;
            out.push_str(c);
        };
        // Feed physical lines (the producer contract).
        for line in text.split_inclusive('\n') {
            stream.push_line(line, &mut sink);
        }
        stream.finish(&mut sink);
        assert!(chunks >= 3, "fixture must really be cut into chunks");
        assert_eq!(out, whole);
        assert_eq!(stream.redactions, report.total);
    }

    #[test]
    fn prefilter_fast_path_returns_clean_input_verbatim() {
        let clean = "{\"a\":\"nothing secret here\"}\n{\"b\":2}\n";
        let (out, rep) = scrub_transcript(clean, &ScrubOptions::secrets_only());
        assert_eq!(out, clean);
        assert_eq!(rep.total, 0);
        let dirty = "{\"a\":\"ghp_abcdefghijklmnopqrstuvwxyz0123456789\"}";
        let (out, rep) = scrub_transcript(dirty, &ScrubOptions::secrets_only());
        assert!(out.contains("[redacted:github-token]") && rep.total == 1);
    }

    // --- boundary-safety property tests (v0.48 SC review) -------------------

    /// One canonical sample per secrets rule: (rule, text, raw secret that must
    /// not survive). Built by concatenation so no real-looking token literal
    /// sits in the source.
    fn canonical_samples() -> Vec<(&'static str, String, String)> {
        let a = |n: usize| "A".repeat(n);
        let mut v: Vec<(&'static str, String, String)> = Vec::new();
        let mut tok = |rule: &'static str, t: String| v.push((rule, t.clone(), t));
        tok(
            "private-key",
            r"-----BEGIN RSA PRIVATE KEY-----\nMIIabc123\n{notastart\n-----END RSA PRIVATE KEY-----"
                .to_string(),
        );
        tok("github-pat", format!("github_pat_{}", a(70)));
        tok("github-token", format!("ghp_{}", a(40)));
        tok("aws-access-key-id", format!("AKIA{}", a(16)));
        tok("slack-token", format!("xoxb-{}", "1234567890abc"));
        tok("google-api-key", format!("AIza{}", a(35)));
        tok("api-key", format!("sk-{}", "QWERTYUIOPASDFGHJKLZXC12"));
        tok("authelia-token", format!("authelia_at_{}", a(24)));
        tok("jwt", "eyJhbGciOi.eyJzdWIiOiIx.SflKxwRJSMeK".to_string());
        let lab = |rule: &'static str, t: &str, s: &str| (rule, t.to_string(), s.to_string());
        v.push(lab(
            "labeled-secret",
            "\"password\": \"hunter2hunter2\"",
            "hunter2hunter2",
        ));
        v.push(lab(
            "labeled-secret",
            "password\n:\n  hunter3hunter3",
            "hunter3hunter3",
        ));
        v.push(lab(
            "labeled-secret",
            "\"token\"\n=\n\"abcdEFGH98765\"",
            "abcdEFGH98765",
        ));
        v.push(lab(
            "bearer-token",
            "Authorization: Bearer abcdefghijklmnop1234",
            "abcdefghijklmnop1234",
        ));
        v.push(lab(
            "bearer-token",
            "Bearer\n\nzyxwvutsrqponmlk9876",
            "zyxwvutsrqponmlk9876",
        ));
        v.push(lab(
            "env-secret",
            "AWS_SECRET_KEY=abc12345xyz",
            "abc12345xyz",
        ));
        v
    }

    fn scrub_unfiltered(text: &str) -> (String, ScrubReport) {
        let mut report = ScrubReport::default();
        let mut t = text.to_string();
        apply_token_rules(&mut t, &mut report);
        apply_group_rules(&mut t, labeled_rules(), &mut report);
        (t, report)
    }

    /// Deterministic xorshift so the generated inputs are reproducible.
    struct Rng(u64);
    impl Rng {
        fn step(&mut self) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 11) as usize
        }
    }

    #[test]
    fn prefilter_set_covers_every_rule_and_never_skips_a_matching_text() {
        let set = secrets_prefilter();
        let rules: Vec<&Regex> = token_rules()
            .iter()
            .map(|r| &r.re)
            .chain(labeled_rules().iter().map(|r| &r.re))
            .collect();
        assert_eq!(set.len(), rules.len(), "set holds every secrets rule");
        for (rule, text, _) in canonical_samples() {
            assert!(set.is_match(&text), "prefilter misses {rule}: {text}");
            let (out, rep) = scrub_transcript(&text, &ScrubOptions::secrets_only());
            let (slow, slow_rep) = scrub_unfiltered(&text);
            assert_eq!(out, slow, "{rule}: fast/slow output");
            assert_eq!(rep, slow_rep, "{rule}: fast/slow report");
            assert!(rep.total >= 1, "{rule} redacted");
        }
        // Clean text: the fast path must equal the slow path's no-op.
        for clean in ["", "{\"a\":1}\n{\"b\":\"x\"}\n", "token\n{\n", "Bearer {\n"] {
            let (slow, rep) = scrub_unfiltered(clean);
            assert_eq!(slow, clean);
            assert_eq!(rep.total, 0);
            assert_eq!(
                scrub_transcript(clean, &ScrubOptions::secrets_only()).0,
                clean
            );
        }
    }

    /// Every rule's sample placed straddling / adjacent to `\n{` boundaries,
    /// inside JSON strings, with whitespace-led labels/bearers ending a line
    /// right before a `{` line: the stream — cutting at EVERY allowed
    /// boundary — equals the whole-text scrub byte for byte, counts match,
    /// and no raw secret survives.
    #[test]
    fn chunked_scrub_equals_whole_text_for_every_rule_at_every_boundary() {
        let samples = canonical_samples();
        let fillers = [
            "{\"pad\":1}\n",
            "{\n",
            "{\"s\":\"a\\nb\"}\n",
            "Bearer\n",
            "token\n",
            "\"password\"\n",
            "password:\n",
            "api_key =\n",
            "\"secret\": \"\n",
            "AWS_SECRET_KEY=\n",
            "   \n",
            "\n",
            "not json line\n",
            "{\"é\":\"日本語\"}\n",
        ];
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        for iter in 0..400 {
            let mut text = String::new();
            let mut must_not_leak: Vec<String> = Vec::new();
            let mut exempt: Vec<String> = Vec::new();
            let n = 4 + rng.step() % 10;
            for _ in 0..n {
                match rng.step() % 4 {
                    0 | 1 => text.push_str(fillers[rng.step() % fillers.len()]),
                    2 => {
                        // The sample as its own JSONL record (JSON string).
                        let (_, t, secret) = &samples[rng.step() % samples.len()];
                        let esc = t.replace('\n', "\\n");
                        text.push_str(&format!("{{\"k\":\"{esc}\"}}\n"));
                        // The escaped form only redacts if it matches as-is;
                        // track the raw secret only for newline-free samples.
                        if !t.contains('\n') {
                            must_not_leak.push(secret.clone());
                        } else {
                            // `\n`-escaped form does not match (by design,
                            // it is not whitespace): the raw value stays.
                            exempt.push(secret.clone());
                        }
                    }
                    _ => {
                        // The sample on bare physical lines, newline-spanning
                        // forms included, followed straight by a `{` record.
                        let (_, t, secret) = &samples[rng.step() % samples.len()];
                        text.push_str("{\"sep\":0}\n");
                        text.push_str(t);
                        text.push('\n');
                        text.push_str("{\"after\":true}\n");
                        must_not_leak.push(secret.clone());
                    }
                }
            }
            let (whole, report) = scrub_transcript(&text, &ScrubOptions::secrets_only());
            for chunk_bytes in [1usize, 17, 200] {
                let mut stream = SecretScrubStream::with_chunk_bytes(chunk_bytes);
                let mut out = String::new();
                let mut sink = |c: &str| out.push_str(c);
                for line in text.split_inclusive('\n') {
                    stream.push_line(line, &mut sink);
                }
                stream.finish(&mut sink);
                assert_eq!(out, whole, "iter {iter} chunk {chunk_bytes}\n{text}");
                assert_eq!(stream.redactions, report.total, "iter {iter} count");
            }
            for secret in must_not_leak.iter().filter(|x| !exempt.contains(x)) {
                assert!(
                    !whole.contains(secret.as_str()),
                    "iter {iter}: {secret} leaked\n{whole}"
                );
            }
        }
    }

    /// A REAL-newline PEM block is not matched by the (non-`(?s)`) private-key
    /// rule in the whole-text scrub; the stream must not behave differently
    /// (neither better nor worse) even when its body has a `\n{` line.
    #[test]
    fn real_newline_pem_with_brace_line_is_chunked_identically() {
        let text = "{\"a\":1}\n-----BEGIN PRIVATE KEY-----\n{inner}\nMIIabc\n-----END PRIVATE KEY-----\n{\"b\":2}\n";
        let (whole, rep) = scrub_transcript(text, &ScrubOptions::secrets_only());
        let mut stream = SecretScrubStream::with_chunk_bytes(1);
        let mut out = String::new();
        let mut sink = |c: &str| out.push_str(c);
        for l in text.split_inclusive('\n') {
            stream.push_line(l, &mut sink);
        }
        stream.finish(&mut sink);
        assert_eq!(out, whole);
        assert_eq!(stream.redactions, rep.total);
    }
}
