//! SEC-17 — a validated revspec TYPE, replacing a validation CONVENTION.
//!
//! # The finding
//!
//! This crate's two prior security fixes were both "a caller-supplied
//! string reached `git`'s option parser": the `/api/diff --output=`
//! arbitrary-WRITE critical, and the `/api/why` `kb_context` leak. The
//! guard that came out of the first is [`crate::reviews::reject_user_ref`]
//! — a free function applied *by convention* at every call site, plus two
//! weaker cousins (`history::reject_dash_prefixed`,
//! `diff::reject_dash_prefixed`) that check only the leading dash. v7 puts
//! a user-supplied ref on *every address*, so "remember to call the
//! validator" scales to dozens of new sites, each one a chance to forget.
//!
//! And the convention has a second failure mode the critique names
//! precisely: `reject_user_ref` REJECTS `..`, which the range/compare
//! features legitimately need — so an implementer reaching for a range is
//! pushed to *bypass* the validator rather than extend it.
//!
//! # The fix
//!
//! Two types whose ONLY constructors are the validators:
//!
//! * [`Revspec`] — one endpoint. Non-empty, no leading `-`, no control or
//!   whitespace character, no `..`, no `@{`. Byte-for-byte the predicate
//!   `reject_user_ref` already enforced (that function now delegates here,
//!   so the two can never drift).
//! * [`RefRange`] — a two-dot or three-dot range, validated
//!   ENDPOINT-WISE. `..` is legal *between* two `Revspec`s and illegal
//!   *inside* one; a type can say that, a string convention cannot.
//!
//! A git helper that takes `&Revspec`/`&RefRange` instead of `&str` cannot
//! be called with an unvalidated string at all: the compiler is the lint.
//!
//! # `--` before pathspecs
//!
//! Separate from validation, and not subsumed by it: every git invocation
//! that passes a caller-supplied PATH must place `--` before it, so a path
//! can never be re-read as a revspec (or vice versa). Enforced at the call
//! sites and asserted by the source-scan lint in
//! `tests/security/git_argv_lint.rs`.

use std::fmt;

/// A validated single revspec — see the module doc.
///
/// Deliberately carries an owned `String`: these are minted once per
/// request from a query param and then handed to a `spawn_blocking`
/// closure, which needs `'static`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Revspec(String);

/// Why a string is not a [`Revspec`]. One variant on purpose — the caller
/// (every route) maps this to one `400`, and enumerating *which* rule a
/// hostile input broke would be a free hint.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid revspec: {0:?}")]
pub struct RevspecError(pub String);

impl Revspec {
    /// The ONLY constructor. Byte-identical to the predicate
    /// `reviews::reject_user_ref` enforced before this type existed:
    /// non-empty, no leading `-` (git's option parser), no control or
    /// whitespace character, no `..` (a range, not an endpoint), no `@{`
    /// (reflog/upstream syntax, which resolves against local state a
    /// caller should not be able to address remotely).
    ///
    /// Does NOT check that the ref exists — that stays the resolver's job
    /// (`reviews::resolve_commit_sha`, `history::resolve_sha`), exactly as
    /// before.
    pub fn parse(s: &str) -> Result<Self, RevspecError> {
        if s.is_empty()
            || s.starts_with('-')
            || s.chars()
                .any(|c| c.is_control() || c.is_whitespace() || c == '\0')
            || s.contains("..")
            || s.contains("@{")
        {
            return Err(RevspecError(s.to_string()));
        }
        Ok(Self(s.to_string()))
    }

    /// A revspec this daemon MINTED itself (a full sha from
    /// `resolve_commit_sha`, a `refs/kbc/review/<id>/ps<n>` built from
    /// daemon integers). Skips validation because the input never came
    /// from a caller — named `trusted` so a call site that is NOT one
    /// reads wrong at review time. Not `pub`: only this crate's own git
    /// helpers may mint one.
    pub(crate) fn trusted(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for Revspec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for Revspec {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl AsRef<std::ffi::OsStr> for Revspec {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.0.as_ref()
    }
}

/// A validated `<from>..<to>` / `<from>...<to>` range — see the module
/// doc. Endpoints are [`Revspec`]s, so `..` can only ever appear as the
/// separator this type inserts, never inside a component.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefRange {
    pub from: Revspec,
    pub to: Revspec,
    /// `true` = `...` (symmetric difference), `false` = `..`.
    pub three_dot: bool,
}

impl RefRange {
    /// Build from two already-validated endpoints.
    pub fn new(from: Revspec, to: Revspec, three_dot: bool) -> Self {
        Self {
            from,
            to,
            three_dot,
        }
    }

    /// Parse a caller-supplied range STRING (`main..topic`,
    /// `main...topic`). Splits on the separator FIRST and validates each
    /// side as a [`Revspec`], which is the whole point: the `..` a range
    /// needs never has to be allowed inside an endpoint.
    ///
    /// The three-dot form is tried first — `a...b` also contains `a..`, so
    /// two-dot-first would mis-split it into (`a`, `.b`).
    pub fn parse(s: &str) -> Result<Self, RevspecError> {
        if let Some((from, to)) = s.split_once("...") {
            return Ok(Self::new(Revspec::parse(from)?, Revspec::parse(to)?, true));
        }
        if let Some((from, to)) = s.split_once("..") {
            return Ok(Self::new(Revspec::parse(from)?, Revspec::parse(to)?, false));
        }
        Err(RevspecError(s.to_string()))
    }

    /// A caller-supplied range that may ALSO be a bare single revspec
    /// (`git range-diff` accepts both `a..b` and a lone `topic`). Falls
    /// back to a degenerate range whose endpoints are equal only when the
    /// input carries no separator at all.
    pub fn parse_loose(s: &str) -> Result<Self, RevspecError> {
        match Self::parse(s) {
            Ok(r) => Ok(r),
            Err(_) if !s.contains("..") => {
                let one = Revspec::parse(s)?;
                Ok(Self::new(one.clone(), one, false))
            }
            Err(e) => Err(e),
        }
    }

    /// The single argv token to hand git. Reassembled from the validated
    /// endpoints, never the caller's original string — so even a parse
    /// that accepted something surprising can only ever emit
    /// `<valid>..<valid>`.
    pub fn as_arg(&self) -> String {
        let sep = if self.three_dot { "..." } else { ".." };
        // A degenerate `parse_loose` range (both endpoints equal, two-dot)
        // came from a caller who typed ONE revspec — hand git that one
        // token back, not a self-range that means "nothing".
        if !self.three_dot && self.from == self.to {
            return self.from.as_str().to_string();
        }
        format!("{}{sep}{}", self.from.as_str(), self.to.as_str())
    }
}

impl fmt::Display for RefRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.as_arg())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revspec_accepts_the_shapes_git_actually_uses() {
        for s in [
            "main",
            "feature/x",
            "HEAD",
            "HEAD~3",
            "HEAD^",
            "v1.2.3",
            "origin/main",
            "75e6a8e",
            "refs/kbc/review/7/ps2",
        ] {
            assert!(Revspec::parse(s).is_ok(), "{s} must parse");
        }
    }

    /// The predicate must stay byte-identical to the `reject_user_ref`
    /// this type replaces — these are that function's own pinned cases
    /// (`reviews::tests::reject_user_ref_blocks_injection_shapes`).
    #[test]
    fn revspec_rejects_every_shape_reject_user_ref_did() {
        for s in ["--output=/tmp/x", "-u", "", "a b", "a..b"] {
            assert!(Revspec::parse(s).is_err(), "{s} must be rejected");
        }
        assert!(Revspec::parse("main@{yesterday}").is_err());
        assert!(Revspec::parse("main\nrm -rf").is_err());
        assert!(Revspec::parse("main\0x").is_err());
    }

    #[test]
    fn a_range_is_validated_endpoint_wise() {
        let r = RefRange::parse("main..topic").unwrap();
        assert_eq!(r.from.as_str(), "main");
        assert_eq!(r.to.as_str(), "topic");
        assert!(!r.three_dot);
        assert_eq!(r.as_arg(), "main..topic");

        let r3 = RefRange::parse("main...topic").unwrap();
        assert!(r3.three_dot);
        assert_eq!(r3.as_arg(), "main...topic");
    }

    #[test]
    fn a_hostile_endpoint_inside_a_range_is_still_rejected() {
        // The exact bypass the critique predicted: `..` is legal as a
        // SEPARATOR, so a naive "allow .. in ranges" relaxation would let
        // `--output=` through on the left.
        assert!(RefRange::parse("--output=/tmp/x..main").is_err());
        assert!(RefRange::parse("main..-u").is_err());
        assert!(RefRange::parse("main..topic@{1}").is_err());
        assert!(RefRange::parse("main.. topic").is_err());
        // Not a range at all.
        assert!(RefRange::parse("main").is_err());
    }

    #[test]
    fn parse_loose_accepts_a_bare_revspec_and_round_trips_it() {
        let r = RefRange::parse_loose("topic").unwrap();
        assert_eq!(r.as_arg(), "topic");
        assert!(RefRange::parse_loose("-u").is_err());
        assert!(RefRange::parse_loose("a..-u").is_err());
        assert_eq!(RefRange::parse_loose("a..b").unwrap().as_arg(), "a..b");
    }

    #[test]
    fn three_dot_is_split_before_two_dot() {
        // `a...b` contains `a..`; a two-dot-first split would produce
        // (`a`, `.b`), and `.b` would then parse as a valid Revspec —
        // a silently WRONG range rather than an error.
        let r = RefRange::parse("a...b").unwrap();
        assert_eq!(r.from.as_str(), "a");
        assert_eq!(r.to.as_str(), "b");
        assert!(r.three_dot);
    }
}
