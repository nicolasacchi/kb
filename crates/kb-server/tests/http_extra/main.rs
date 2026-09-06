//! In-process HTTP extras: coderefs, exclusions, events filters, SLOs,
//! CT-F3 unlinked mentions, SL2 slates.
//!
//! Filter example: `cargo test -p kb-server --test http_extra events_filters::`

#[allow(unused)]
#[path = "../common/mod.rs"]
mod common;

mod coderefs;
mod events_filters;
mod exclusions;
mod links_suggest;
mod slates;
mod slo;
