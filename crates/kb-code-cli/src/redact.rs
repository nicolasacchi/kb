//! V70-A8 (D20 secret hygiene) — the ONE place a resolved secret gets
//! scrubbed before it can reach a `--dry-run` render (or any other
//! echoed-back-to-the-operator string this CLI ever prints).
//!
//! `kb-code scip run --dry-run` is, today, the only verb that echoes an
//! operator-authored invocation (`[[scip.repos]] command`/`output`/the
//! repo's `cwd`) back to the terminal before running it. None of those
//! values is *expected* to carry this CLI's own resolved bearer token —
//! but an operator's indexer command COULD embed a secret-shaped flag
//! (their own, unrelated to `kb-code`'s), and a config-driven string is
//! exactly the kind of value that quietly grows one over time. Scrubbing
//! this CLI's OWN resolved token here is the concrete, testable half of
//! that guarantee: whatever else a `--dry-run` render might one day leak,
//! it will never be the token THIS invocation was carrying.

/// The fixed marker substituted for a redacted secret. Fixed (not
/// abbreviated, not partially shown) — a partial reveal is still a leak.
pub const REDACTED_MARKER: &str = "***REDACTED***";

/// Replace every occurrence of `secret` in `text` with [`REDACTED_MARKER`].
/// A `None`/empty secret is a no-op (nothing resolved, nothing to scrub).
pub fn scrub(text: &str, secret: Option<&str>) -> String {
    match secret {
        Some(s) if !s.is_empty() => text.replace(s, REDACTED_MARKER),
        _ => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrubs_every_occurrence() {
        let out = scrub("token=abc123 and again abc123", Some("abc123"));
        assert_eq!(out, "token=***REDACTED*** and again ***REDACTED***");
    }

    #[test]
    fn no_secret_is_a_no_op() {
        assert_eq!(scrub("nothing to hide", None), "nothing to hide");
        assert_eq!(scrub("nothing to hide", Some("")), "nothing to hide");
    }
}
