//! V3.G2 — pure heuristics that score/classify resolution candidates and
//! usage rows. Never gates (never drops a candidate); only demotes class
//! or reorders rank. Submodules:
//!
//! - [`arity`] — call-site argument count vs symbol param range; receiver
//!   type-name boost when the locals graph exposes a syntactically evident
//!   type.
//! - [`access`] — read/write tagging of a reference token (assignment LHS,
//!   compound assignment, `&mut`, `del` target).
//! - [`cross_file`] — Sourcegraph-style import-reachability filter + fuzzy
//!   fallback for the tags-tier cross-file arm of `resolve` / `usages`.
//! - [`ruby_strict`] — V71-E1: Ruby same-file locals binding under D4's
//!   STRICT rule (the one door to `exact` for Ruby), plus the enclosing
//!   constant hierarchy the rule's clause (b) is evaluated against.
//! - [`usekind`] — V71-E1: `usages/2`'s `kind` from the CST, one parse per
//!   file, `None` rather than a guess.

pub mod access;
pub mod arity;
pub mod cross_file;
pub mod ruby_strict;
pub mod usekind;
