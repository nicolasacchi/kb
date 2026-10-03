//! `Posture`: a security-relevant enum names its most restrictive value, and a
//! test proves `Default` IS that value (v0.44 X5, F1 carry).
//!
//! A `#[derive(Default)]` enum whose first-listed variant happens to be the
//! permissive one is a fail-open default that no compiler or lint notices; a
//! reorder of variants silently flips it. Implementing [`Posture`] forces the
//! author to state which variant is restrictive, and
//! [`assert_default_is_restrictive`] (called from a per-type test) turns that
//! statement into a failing test when `Default` drifts.
//!
//! INVENTORY of defaulted enums reviewed for this (every `#[default]` enum in
//! the workspace, 2026-10):
//!
//! * `review::Visibility` -- implements `Posture` (`Public`, never `All`).
//! * `slate::Origin` (`Agent`) -- not a restrictive/permissive axis; the
//!   property that matters is "the default is not `Human`" (the only value that
//!   earns the `[you]` rendering and the pin check), pinned in slate's tests.
//! * `review_store::cred::CredentialPin` (`Auto`) -- the permissive credential
//!   modes (`Inherit`) are additionally gated by the separate
//!   `allow_inherited_credentials` bool, whose `false` default is pinned in
//!   kb-code-server's config tests; `Auto` is a selector, not a grant.
//! * `ForgeKind`, `LinksMode`, `Mode`, `BranchSort`, `AtlasLayoutChoice`,
//!   `DecayPolicy`, the `docs_query`/`lists`/`parser`/`vcs`/`watcher` enums --
//!   selectors with no access-control meaning; not security-relevant.
//!
//! A NEW defaulted enum that gates access (who may see, write or reach
//! something) implements `Posture` and adds its test here or next to it.

use std::fmt::Debug;

/// A security-relevant type with one most-restrictive value.
pub trait Posture: Default + PartialEq + Debug + Sized {
    /// The value that grants the least access / reveals the least.
    fn restrictive() -> Self;
}

/// True when `T::default()` is `T::restrictive()`.
pub fn default_is_restrictive<T: Posture>() -> bool {
    T::default() == T::restrictive()
}

/// Panics, naming both values, unless `T::default()` is `T::restrictive()`.
pub fn assert_default_is_restrictive<T: Posture>() {
    assert!(
        default_is_restrictive::<T>(),
        "{}: Default is {:?} but the restrictive value is {:?} -- a fail-open default",
        std::any::type_name::<T>(),
        T::default(),
        T::restrictive()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, Default)]
    enum Open {
        #[default]
        Wide,
        #[allow(dead_code)]
        Narrow,
    }
    impl Posture for Open {
        fn restrictive() -> Self {
            Open::Narrow
        }
    }

    #[derive(Debug, PartialEq, Default)]
    enum Closed {
        #[default]
        Narrow,
        #[allow(dead_code)]
        Wide,
    }
    impl Posture for Closed {
        fn restrictive() -> Self {
            Closed::Narrow
        }
    }

    #[test]
    fn a_permissive_default_is_detected() {
        assert!(!default_is_restrictive::<Open>());
        assert!(default_is_restrictive::<Closed>());
        assert_default_is_restrictive::<Closed>();
    }

    #[test]
    #[should_panic(expected = "fail-open default")]
    fn assert_panics_on_a_permissive_default() {
        assert_default_is_restrictive::<Open>();
    }
}
