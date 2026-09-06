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

pub mod access;
pub mod arity;
pub mod cross_file;
