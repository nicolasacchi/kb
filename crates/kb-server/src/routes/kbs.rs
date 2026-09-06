//! `GET /api/kbs` — list configured kbs with doc counts (topic 11 §B.1).

use crate::state::KbHandles;
use axum::{extract::State, Json};
use serde::Serialize;
use std::sync::Arc;

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct KbSummary {
    pub name: String,
    pub path: String,
    pub doc_count: u64,
    pub last_index_at: Option<i64>,
    /// v0.9 M2 — `"global"` | `"project"` when this is a memory corpus;
    /// `None` for ordinary artifact corpora. Lets the SPA render the
    /// `/memory` view distinct from kb-docs/research.
    pub memory_scope: Option<String>,
    /// R0-opt-in — `[kb.foo] default_search_category` (e.g.
    /// `"memory-session"` on a sessions corpus). `None` when this kb has no
    /// configured default. Lets the SPA offer a one-click "make searchable
    /// by default" affordance when this kb is directly scoped, and explain
    /// why session transcripts already show up without typing
    /// `?category=memory-session`.
    pub default_search_category: Option<String>,
    /// DCB — `[kb.foo] code_url` (e.g. `https://kbc.example.com`), the kb-code
    /// daemon that indexes the repos this corpus's docs cite. `None` when
    /// this kb isn't linked to one — the SPA's Code section then renders
    /// extracted refs as inert rows ("not linked to a code repo") instead of
    /// resolving them. kb itself never opens an HTTP client to this URL
    /// (invariant #2/#4 — one live call direction, kb-code→kb).
    pub code_url: Option<String>,
}

pub async fn list(State(state): State<Arc<KbHandles>>) -> Json<Vec<KbSummary>> {
    // FF-E — fan out the per-kb summary reads concurrently (bounded), collecting
    // in BTreeMap order. Pure reads.
    let mut futs: Vec<super::CorpusFut<'_, KbSummary>> = Vec::new();
    for (name, ctx) in &state.kbs {
        futs.push(Box::pin(async move {
            let doc_count = ctx.storage.count_rows().await.unwrap_or(0);
            let last_index_at = ctx
                .storage
                .last_run_for_source(ctx.source_slug.clone())
                .await
                .ok()
                .flatten()
                .and_then(|r| r.finished_at_unix);
            KbSummary {
                name: name.to_string(),
                path: ctx.source_path.to_string_lossy().to_string(),
                doc_count,
                last_index_at,
                memory_scope: ctx.memory_scope.clone(),
                default_search_category: ctx.default_search_category.clone(),
                code_url: ctx.code_url.clone(),
            }
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let summaries = super::buffered_join(futs, state.fanout_cap).await;
    Json(summaries)
}
