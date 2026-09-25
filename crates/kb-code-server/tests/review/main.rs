//! Review / local-review / review-map HTTP harness.
//!
//! Filter example: `cargo test -p kb-code-server --test review`

#[allow(unused)]
#[path = "../common/mod.rs"]
mod common;

/// ONE binary-wide serializer for tests that mutate the process-global
/// `KB_CODE_TOKEN` env var. Three modules (`review_comments`,
/// `review_findings`, `remote_mutations_gate`) each used to carry a
/// PRIVATE `SERIAL` mutex — correct within a file, but their bearer-auth
/// tests raced ACROSS files under parallel test threads (set-token /
/// unset-token interleaving), failing only on loaded boxes. Every
/// env-mutating test must lock THIS mutex, never a module-local one.
pub(crate) static ENV_SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

mod annotate_bind;
mod local_review_routes;
mod remote_mutations_gate;
mod review_analytics;
mod review_comments;
mod review_distill;
mod review_doc;
mod review_findings;
mod review_github_export;
mod review_github_threads;
mod review_impact;
mod review_inbox_timeline;
mod review_map_route;
mod review_routes;
mod review_sweep;
mod rs_u0_golden;
mod rs_u10a_views;
mod rs_u11a_golden_envelopes;
mod rs_u7_retrack;
mod shape_util;
mod unified_inbox;
mod v73_k3;
