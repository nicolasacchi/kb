//! Multi-user attribution (kb-users/1) — pure username helpers.
//!
//! Users are ATTRIBUTION strings, not authorization principals. Every
//! identity ladder (phase Y server header → token → operator) resolves
//! through [`normalize_username`] so case-forked history rows cannot
//! accumulate (append-only history makes case forks permanent).
//!
//! Config-side values ([`crate::config::IdentitySection`]) must already
//! be lowercase-valid — `validate()` hard-rejects uppercase rather than
//! silently folding.

use serde::{Deserialize, Serialize};

/// Default `[identity].operator` and the attribution stamped when no
/// other identity is known (loopback, legacy shared-token callers).
pub const DEFAULT_OPERATOR: &str = "operator";

/// Default trusted-identity header name (Traefik/Authelia style).
pub const DEFAULT_HEADER: &str = "Remote-User";

/// Max length of a username (config + tokens + stamped columns).
pub const USERNAME_MAX_LEN: usize = 64;

/// True iff `s` is a valid *already-normalized* username:
/// `^[a-z0-9._@-]{1,64}$` (lowercase only — case forks in append-only
/// history are permanent, so the ladder never accepts mixed case).
pub fn username_is_valid(s: &str) -> bool {
    let len = s.len();
    if len == 0 || len > USERNAME_MAX_LEN {
        return false;
    }
    s.bytes()
        .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'@' | b'-'))
}

/// Trim, lowercase-fold (ASCII), then validate. Returns `None` when the
/// result is empty, too long, or contains a non-username character.
/// The phase-Y identity ladder resolves EVERY inbound identity through
/// this function.
pub fn normalize_username(s: &str) -> Option<String> {
    let trimmed = s.trim();
    if trimmed.is_empty() || trimmed.len() > USERNAME_MAX_LEN {
        return None;
    }
    let folded: String = trimmed
        .chars()
        .map(|c| {
            if c.is_ascii_uppercase() {
                c.to_ascii_lowercase()
            } else {
                c
            }
        })
        .collect();
    if username_is_valid(&folded) {
        Some(folded)
    } else {
        None
    }
}

/// True iff `name` is a valid HTTP header-field token (RFC 7230 §3.2.6):
/// `tchar = "!" / "#" / "$" / "%" / "&" / "'" / "*" / "+" / "-" / "." /
///  "^" / "_" / "`" / "|" / "~" / DIGIT / ALPHA`. Used to validate
/// `[identity].header`.
pub fn header_name_is_valid(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|b| {
            matches!(
                b,
                b'!'
                    | b'#'
                    | b'$'
                    | b'%'
                    | b'&'
                    | b'\''
                    | b'*'
                    | b'+'
                    | b'-'
                    | b'.'
                    | b'^'
                    | b'_'
                    | b'`'
                    | b'|'
                    | b'~'
                    | b'0'..=b'9'
                    | b'A'..=b'Z'
                    | b'a'..=b'z'
            )
        })
}

/// How a token secret is stored in the tokens file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenSecret {
    /// `sha256:<hex>` — compare against SHA-256 of the presented bearer.
    Sha256(String),
    /// Raw plaintext secret (dev / simple deployments).
    Plain(String),
}

/// One `<user>:<secret>` line from a tokens file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenEntry {
    pub user: String,
    pub secret: TokenSecret,
}

/// Parse a tokens file body into entries. One entry per non-blank,
/// non-`#` line; forms:
/// - `<user>:sha256:<hex>`
/// - `<user>:<plaintext>`
///
/// Invalid user (must already be lowercase-valid) or empty secret → skip
/// (caller logs). Pure + unit-tested.
pub fn parse_tokens_file(body: &str) -> Vec<TokenEntry> {
    let mut out = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((user_raw, rest)) = line.split_once(':') else {
            continue;
        };
        let user_raw = user_raw.trim();
        // Tokens file users must already be lowercase-valid — same rule
        // as config. Do NOT silently fold: an uppercase user is a
        // misconfiguration the operator must fix.
        if !username_is_valid(user_raw) {
            continue;
        }
        let rest = rest.trim();
        if rest.is_empty() {
            continue;
        }
        let secret = if let Some(hex) = rest.strip_prefix("sha256:") {
            let hex = hex.trim();
            if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                continue;
            }
            TokenSecret::Sha256(hex.to_ascii_lowercase())
        } else {
            TokenSecret::Plain(rest.to_string())
        };
        out.push(TokenEntry {
            user: user_raw.to_string(),
            secret,
        });
    }
    out
}

/// Optional display metadata for a known user in `[identity.users]`.
/// Unknown users still attribute verbatim via the header/token ladder —
/// this table is cosmetic only.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct IdentityUser {
    /// Lowercase-valid username (hard-validated at config load).
    pub name: String,
    /// Optional display label for the SPA.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn username_is_valid_accepts_dots_dashes_at() {
        assert!(username_is_valid("operator"));
        assert!(username_is_valid("a"));
        assert!(username_is_valid("user.name"));
        assert!(username_is_valid("user-name"));
        assert!(username_is_valid("user_name"));
        assert!(username_is_valid("user@host"));
        assert!(username_is_valid("a1.b2-c3_d4@e5"));
        assert!(username_is_valid(&"a".repeat(64)));
    }

    #[test]
    fn username_is_valid_rejects_empty_long_space_unicode_upper() {
        assert!(!username_is_valid(""));
        assert!(!username_is_valid(&"a".repeat(65)));
        assert!(!username_is_valid("has space"));
        assert!(!username_is_valid("has\ttab"));
        assert!(!username_is_valid("üser"));
        assert!(!username_is_valid("User"));
        assert!(!username_is_valid("OPERATOR"));
        assert!(!username_is_valid("user:name"));
        assert!(!username_is_valid("user/name"));
        assert!(!username_is_valid(" user"));
    }

    #[test]
    fn normalize_username_trims_and_folds() {
        assert_eq!(normalize_username("  Alice  ").as_deref(), Some("alice"));
        assert_eq!(normalize_username("OPERATOR").as_deref(), Some("operator"));
        assert_eq!(
            normalize_username("a.B-c_D@E").as_deref(),
            Some("a.b-c_d@e")
        );
        assert_eq!(normalize_username("").as_deref(), None);
        assert_eq!(normalize_username("   ").as_deref(), None);
        assert_eq!(normalize_username("has space").as_deref(), None);
        assert_eq!(normalize_username(&"A".repeat(65)).as_deref(), None);
        assert_eq!(normalize_username("üser").as_deref(), None);
    }

    #[test]
    fn header_name_is_valid_token() {
        assert!(header_name_is_valid("Remote-User"));
        assert!(header_name_is_valid("X-Forwarded-User"));
        assert!(header_name_is_valid("X-Auth-Request-User"));
        assert!(!header_name_is_valid(""));
        assert!(!header_name_is_valid("Remote User"));
        assert!(!header_name_is_valid("Remote:User"));
        assert!(!header_name_is_valid("Remote/User"));
    }

    #[test]
    fn parse_tokens_file_both_forms_comments_malformed() {
        let body = r#"
# comment
operator:sha256:deadbeefCAFE
alice:s3cret
bob:sha256:
# uppercase user rejected
Charlie:plain
:noser
nouser
dave:sha256:not-hex!
 eve : plaintext-with-spaces 

"#;
        let entries = parse_tokens_file(body);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].user, "operator");
        assert_eq!(
            entries[0].secret,
            TokenSecret::Sha256("deadbeefcafe".into())
        );
        assert_eq!(entries[1].user, "alice");
        assert_eq!(entries[1].secret, TokenSecret::Plain("s3cret".into()));
        assert_eq!(entries[2].user, "eve");
        assert_eq!(
            entries[2].secret,
            TokenSecret::Plain("plaintext-with-spaces".into())
        );
    }

    #[test]
    fn parse_tokens_file_empty_and_blanks() {
        assert!(parse_tokens_file("").is_empty());
        assert!(parse_tokens_file("\n\n# only comments\n").is_empty());
    }
}
