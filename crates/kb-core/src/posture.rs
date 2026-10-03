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
//! The INVENTORY of defaulted enums is code, not prose:
//! `crates/kb-core/tests/posture_inventory.rs` walks every workspace
//! `#[default]` enum and fails when one is neither a `Posture` implementor
//! (with `assert_default_is_restrictive` pinning it) nor listed there with
//! the reason it is not an access-control axis. A NEW defaulted enum that
//! gates access (who may see, write or reach something) implements
//! `Posture`, adds its pinning test, and is listed `Posture` in that table.

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
