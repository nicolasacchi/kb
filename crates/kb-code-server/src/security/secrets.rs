//! SEC-13 (b) + the critique's MISSING #5 — the server-enforced secret
//! denylist and the `redaction_hint` content sniff.
//!
//! # Why the server, not the UI
//!
//! The v7 design puts a "config/secret triptych" in the reader and turns
//! the tree into a working-tree walk, so `.env`, `config/master.key`,
//! `*.pem` and friends stop being *guessable* and become *listed*. The
//! critique's fix is explicit that "name-only" must be a SERVER property:
//! `GET /api/file` is directly addressable, so a UI that merely declines
//! to render a file is not a control at all.
//!
//! So the policy lives here, at the read, and refuses with a typed
//! `urn:kb:errors:redacted-by-policy` 403 that NAMES THE MATCHED PATTERN
//! and never the bytes. Naming the pattern is deliberate: the caller
//! (human or agent) needs to know *why* a read was refused in order to
//! either stop asking or widen the policy on purpose, and the pattern is
//! information the operator already wrote down.
//!
//! # Additive-only
//!
//! [`BUILTIN_SECRET_GLOBS`] is the floor. `[security] secret_globs` ADDS
//! to it and can never subtract, so no repo-resident or per-deployment
//! config can re-open `.env`. (An operator who genuinely needs to read a
//! denylisted file has the file itself, on the same machine — this daemon
//! is not the only way to `cat` it.)
//!
//! # The sniff is a HINT, never a redaction
//!
//! [`redaction_hint`] looks for private-key headers, AWS-shaped access
//! keys and `password=`/`secret=` assignments in served TEXT. It does NOT
//! redact and does NOT refuse: a heuristic that silently mangled source
//! would be a correctness bug in a code reader (`AWS_ACCESS_KEY_ID` is a
//! perfectly ordinary string to read *about* in a docs file), and the
//! `exact`-honesty discipline this codebase applies to trust classes
//! applies just as well to a security banner. It sets one additive
//! `redaction_hint: true` field on the file wire; the SPA decides what to
//! say about it.

use crate::routes::ApiError;
use axum::http::StatusCode;
use std::sync::OnceLock;

/// `urn:kb:errors:redacted-by-policy`.
pub const ERR_REDACTED_BY_POLICY: &str = "urn:kb:errors:redacted-by-policy";

/// The built-in denylist floor — the exact set the V70-A2 brief names.
///
/// A pattern containing `/` is matched against the repo-relative PATH;
/// every other pattern is matched against the BASENAME (so `*.pem` covers
/// `certs/deep/server.pem`). `*` never crosses a `/`.
pub const BUILTIN_SECRET_GLOBS: &[&str] = &[
    ".env",
    ".env.*",
    "*.key",
    "*.pem",
    "*.p12",
    "*.pfx",
    "id_rsa*",
    "config/master.key",
    "config/credentials/*",
    "*.sqlite3",
    "*.keystore",
];

/// The compiled policy: the built-ins plus `[security] secret_globs`.
/// Built once at boot and parked on [`crate::state::AppState`] (no live
/// reload — same posture as every other config clone there).
#[derive(Debug, Clone)]
pub struct SecretPolicy {
    globs: Vec<String>,
}

impl Default for SecretPolicy {
    fn default() -> Self {
        Self::new(&[])
    }
}

impl SecretPolicy {
    /// `BUILTIN_SECRET_GLOBS` ∪ `extra`, de-duplicated, order-stable
    /// (built-ins first) so the pattern NAMED in a refusal is stable
    /// across boots.
    pub fn new(extra: &[String]) -> Self {
        let mut globs: Vec<String> = BUILTIN_SECRET_GLOBS.iter().map(|s| s.to_string()).collect();
        for e in extra {
            let e = e.trim();
            if !e.is_empty() && !globs.iter().any(|g| g == e) {
                globs.push(e.to_string());
            }
        }
        Self { globs }
    }

    pub fn globs(&self) -> &[String] {
        &self.globs
    }

    /// The FIRST pattern `rel` (a repo-relative, forward-slash path)
    /// matches, or `None`.
    pub fn matched(&self, rel: &str) -> Option<&str> {
        let normalized = rel.trim_start_matches("./").replace('\\', "/");
        let basename = normalized.rsplit('/').next().unwrap_or(&normalized);
        self.globs
            .iter()
            .find(|g| {
                if g.contains('/') {
                    glob_match(g, &normalized)
                } else {
                    glob_match(g, basename)
                }
            })
            .map(|g| g.as_str())
    }

    /// `Ok(())` or the typed 403. The one call every content-returning
    /// read site makes.
    pub fn check(&self, rel: &str) -> Result<(), ApiError> {
        match self.matched(rel) {
            None => Ok(()),
            Some(pattern) => Err(redacted(rel, pattern)),
        }
    }
}

/// The BUILT-IN FLOOR as a shared, immutable static.
///
/// # The two-level enforcement split, and why
///
/// `routes::read_repo_file` is THE working-tree/ODB read in this crate —
/// 24 call sites across ten modules (`lip`, `usages`, `hierarchy`,
/// `resolve`, `lenses`, `impact_analysis`, `code_actions`, …), most of
/// which hold no `SharedState`. So the policy is enforced at two levels:
///
/// * **The floor, everywhere.** [`builtin_policy`] is derived from a
///   `const` and carries no configuration, so every read site — including
///   the ones that never see `AppState` — enforces
///   [`BUILTIN_SECRET_GLOBS`] with no parameter to thread and none to
///   forget. This is the guarantee: `.env`, `config/master.key`, `*.pem`
///   and the rest are unreadable through this daemon, full stop.
/// * **The operator's additions, at the addressable surface.**
///   `[security] secret_globs` lands on [`crate::state::AppState::
///   secret_policy`] and is checked by the routes that hold state and
///   RETURN CONTENT to a caller — `GET /api/file` and `GET /api/pack`.
///   Those are exactly the two surfaces SEC-13 names ("the read route is
///   directly addressable") and the two the critique's MISSING #5 names
///   ("secrets flowing into `pack` output that an agent then writes to a
///   transcript").
///
/// The alternative — one process-global holding the CONFIGURED policy —
/// was written and rejected: a `OnceLock` would let the first daemon
/// booted in a test process fix the policy for every later one, and a
/// `RwLock` would make a parallel test's boot silently change another
/// test's policy. A security control whose value depends on boot ORDER is
/// worse than a two-level split that is written down.
static BUILTIN: OnceLock<SecretPolicy> = OnceLock::new();

/// The floor — see the split above. Never configurable, never mutable.
pub fn builtin_policy() -> &'static SecretPolicy {
    BUILTIN.get_or_init(SecretPolicy::default)
}

/// The typed refusal — names the matched PATTERN, never the bytes.
pub fn redacted(rel: &str, pattern: &str) -> ApiError {
    ApiError::new(
        StatusCode::FORBIDDEN,
        format!("{rel}: redacted by policy (matched secret glob {pattern:?})"),
    )
    .with_problem_type(ERR_REDACTED_BY_POLICY)
}

/// Segment-aware `*` glob. `*` matches any run of characters EXCEPT `/`;
/// there is no `**`, no `?`, no character class.
///
/// Deliberately its own ~20 lines rather than `crate::scopes::
/// path_matches_any`: that matcher is a deliberately-loose *superset*
/// helper (it falls through to substring matching for a literal pattern,
/// and its `star_match` lets `*` cross `/`), which is the right shape for
/// a user's "which files count as tests" scope and exactly the wrong shape
/// for a security denylist, where a pattern must mean precisely one thing
/// and be readable as such in a refusal message. No new dependency either
/// way — `globset` is not in the workspace lockfile (`scopes.rs`'s own
/// module doc records that).
fn glob_match(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    // A `*` may not cross a `/`: check the literal anchors, then require
    // that the spans the wildcards cover contain no separator.
    let first = parts[0];
    let last = parts[parts.len() - 1];
    if !text.starts_with(first) || !text.ends_with(last) {
        return false;
    }
    if text.len() < first.len() + last.len() {
        return false;
    }
    let mut pos = first.len();
    let end = text.len() - last.len();
    for part in &parts[1..parts.len() - 1] {
        if part.is_empty() {
            continue;
        }
        match text[pos..end].find(part) {
            Some(off) => {
                if text[pos..pos + off].contains('/') {
                    return false;
                }
                pos += off + part.len();
            }
            None => return false,
        }
    }
    !text[pos..end].contains('/')
}

/// A cheap content sniff over served TEXT — see the module doc. Never
/// redacts; the caller sets one additive `redaction_hint` wire field.
///
/// Scans a bounded prefix (`SNIFF_CAP` bytes) so a 5 MB minified bundle
/// costs the same as a 40-line `.env.example`.
pub fn redaction_hint(text: &str) -> bool {
    let mut cap = text.len().min(SNIFF_CAP);
    while cap < text.len() && !text.is_char_boundary(cap) {
        cap -= 1;
    }
    let head = &text[..cap];
    if head.contains("-----BEGIN") && head.contains("PRIVATE KEY-----") {
        return true;
    }
    if looks_like_aws_access_key(head) {
        return true;
    }
    assignment_hint(head)
}

/// 64 KiB — enough to cover a config file whole and the head of anything
/// larger, small enough that the scan never shows up in a read's latency.
const SNIFF_CAP: usize = 64 * 1024;

/// `AKIA`/`ASIA`-style: an `AWS`-prefixed 16+ char uppercase-alnum run.
/// Written as a hand-rolled scan rather than a `regex::Regex` so it costs
/// no lazy-static compile on a path every file read takes.
fn looks_like_aws_access_key(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut i = 0;
    while let Some(off) = text[i..].find("AWS") {
        let start = i + off;
        let mut run = 0;
        let mut j = start + 3;
        while j < bytes.len() && (bytes[j].is_ascii_uppercase() || bytes[j].is_ascii_digit()) {
            run += 1;
            j += 1;
        }
        if run >= 16 {
            return true;
        }
        i = start + 3;
        if i >= text.len() {
            break;
        }
    }
    false
}

/// `password=`/`passwd=`/`secret=`/`api_key=`/`token=` with a non-empty,
/// non-placeholder value on the right — the shape a real credential
/// assignment has in a `.env`-like, YAML or `.properties` file.
///
/// The key must be a LINE-LEADING bare identifier (optionally
/// `export`-prefixed), which is what keeps ordinary source out: `let
/// password = read()` and `if (secret == x)` both have a left side that is
/// not an identifier, so neither is a hint. Being wrong in this direction
/// is the point — see the module doc's "the sniff is a HINT" section.
fn assignment_hint(text: &str) -> bool {
    const KEYS: &[&str] = &["password", "passwd", "secret", "api_key", "apikey", "token"];
    for line in text.lines() {
        let lower = line.trim_start().to_ascii_lowercase();
        let lower = lower.strip_prefix("export ").unwrap_or(&lower).trim_start();
        let Some(eq) = lower.find(['=', ':']) else {
            continue;
        };
        let key = lower[..eq].trim().trim_matches(['"', '\'']);
        if key.is_empty()
            || !key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        {
            continue;
        }
        if !KEYS.iter().any(|k| key.ends_with(k)) {
            continue;
        }
        let value = lower[eq + 1..].trim().trim_matches(['"', '\'', ',', ';']);
        // A placeholder, an interpolation, or an empty value is what a
        // checked-in EXAMPLE looks like — not a hint worth raising.
        if value.len() < 8
            || value.starts_with("${")
            || value.starts_with("env[")
            || value.starts_with("<%")
            || value.contains("change")
            || value.contains("xxx")
            || value.contains("...")
        {
            continue;
        }
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_builtin_floor_is_the_documented_set() {
        let p = SecretPolicy::default();
        for (path, pattern) in [
            (".env", ".env"),
            ("app/.env", ".env"),
            (".env.production", ".env.*"),
            ("config/master.key", "*.key"),
            ("certs/deep/server.pem", "*.pem"),
            ("cert.p12", "*.p12"),
            ("cert.pfx", "*.pfx"),
            (".ssh/id_rsa", "id_rsa*"),
            ("id_rsa.pub", "id_rsa*"),
            ("config/credentials/production.yml", "config/credentials/*"),
            ("db/dev.sqlite3", "*.sqlite3"),
            ("app.keystore", "*.keystore"),
        ] {
            assert_eq!(p.matched(path), Some(pattern), "{path}");
        }
    }

    #[test]
    fn ordinary_source_is_never_denylisted() {
        let p = SecretPolicy::default();
        for path in [
            "src/main.rs",
            "app/models/user.rb",
            "README.md",
            "config/routes.rb",
            "docs/env.md",
            "keystore.rs",
            "web/src/api/client.ts",
        ] {
            assert_eq!(p.matched(path), None, "{path}");
        }
    }

    #[test]
    fn config_secret_globs_are_additive_never_subtractive() {
        let p = SecretPolicy::new(&["*.enc".to_string(), ".env".to_string()]);
        // The extra pattern applies...
        assert_eq!(p.matched("vault/prod.enc"), Some("*.enc"));
        // ...and the built-in floor is untouched, listed once.
        assert_eq!(p.matched(".env"), Some(".env"));
        assert_eq!(p.globs().iter().filter(|g| *g == ".env").count(), 1);
        assert_eq!(p.globs().len(), BUILTIN_SECRET_GLOBS.len() + 1);
    }

    #[test]
    fn a_star_never_crosses_a_directory_separator() {
        // `config/credentials/*` must not reach a nested directory.
        let p = SecretPolicy::default();
        assert_eq!(
            p.matched("config/credentials/prod.key.enc"),
            Some("config/credentials/*")
        );
        assert!(!glob_match(
            "config/credentials/*",
            "config/credentials/a/b"
        ));
        assert!(glob_match("*.pem", "server.pem"));
        assert!(!glob_match("*.pem", "certs/server.pem"));
    }

    #[test]
    fn the_floor_needs_no_boot_and_no_configuration() {
        // The fail-SAFE direction: a code path that never saw an
        // `AppState` still refuses `.env` (see the two-level split above).
        assert_eq!(builtin_policy().matched(".env"), Some(".env"));
        assert_eq!(builtin_policy().matched("src/main.rs"), None);
    }

    #[test]
    fn the_refusal_names_the_pattern_and_never_the_bytes() {
        let p = SecretPolicy::default();
        let err = p.check("config/master.key").unwrap_err();
        assert_eq!(err.status_code(), StatusCode::FORBIDDEN);
        assert_eq!(err.problem_type(), Some(ERR_REDACTED_BY_POLICY));
        assert!(err.message().contains("*.key"), "{}", err.message());
    }

    #[test]
    fn the_sniff_flags_real_shapes_and_not_examples() {
        assert!(redaction_hint(
            "-----BEGIN OPENSSH PRIVATE KEY-----\nabc\n-----END"
        ));
        assert!(redaction_hint("AWSACCESSKEYID0000AB = x"));
        assert!(redaction_hint("DATABASE_PASSWORD=hunter2isnotgreat"));
        assert!(redaction_hint("api_key: 91acf30bd77e4b6a9c11"));

        assert!(!redaction_hint("fn main() { let password = read(); }"));
        assert!(!redaction_hint("PASSWORD=${DB_PASSWORD}"));
        assert!(!redaction_hint("secret: CHANGE_ME_PLEASE"));
        assert!(!redaction_hint("token = xxx"));
        assert!(!redaction_hint("# AWS is a cloud provider"));
        assert!(!redaction_hint(""));
    }

    #[test]
    fn the_sniff_is_bounded() {
        // A 1 MiB file whose only hint is past the cap is not flagged —
        // the scan is bounded by construction, not by luck.
        let mut s = "a".repeat(SNIFF_CAP + 10);
        s.push_str("-----BEGIN RSA PRIVATE KEY-----");
        assert!(!redaction_hint(&s));
    }
}
