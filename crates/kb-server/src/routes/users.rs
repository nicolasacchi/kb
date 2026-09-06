//! `GET /api/users` — configured ∪ observed users (v0.34 Y1 / kb-users/1).
//!
//! Shape:
//! ```json
//! { "users": [{ "name", "display", "configured", "observed" }] }
//! ```
//!
//! Configured = `[identity].operator` ∪ `[identity.users]`. Observed =
//! `SELECT DISTINCT user FROM history` across every kb (union). Comment
//! users are NOT merged in v1 (note for Z/W).

use crate::state::KbHandles;
use axum::{extract::State, response::IntoResponse, Json};
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::Arc;

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct UserEntry {
    pub name: String,
    /// Optional display label from `[identity.users]`; absent for
    /// observed-only users (and for operator unless listed).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub display: Option<String>,
    pub configured: bool,
    pub observed: bool,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct UsersResponse {
    pub users: Vec<UserEntry>,
}

pub async fn list(State(state): State<Arc<KbHandles>>) -> impl IntoResponse {
    // Seed from config (operator + known users).
    let mut map: BTreeMap<String, UserEntry> = BTreeMap::new();
    {
        let cfg = state.config.read().await;
        let operator = cfg.identity.operator.clone();
        map.insert(
            operator.clone(),
            UserEntry {
                name: operator,
                display: None,
                configured: true,
                observed: false,
            },
        );
        for u in &cfg.identity.users {
            map.entry(u.name.clone())
                .and_modify(|e| {
                    e.configured = true;
                    if e.display.is_none() {
                        e.display = u.display.clone();
                    }
                })
                .or_insert_with(|| UserEntry {
                    name: u.name.clone(),
                    display: u.display.clone(),
                    configured: true,
                    observed: false,
                });
        }
    }

    // Observed: DISTINCT history.user across every kb (fan-out, #28).
    let mut futs: Vec<super::CorpusFut<'_, Vec<String>>> = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        futs.push(Box::pin(async move {
            match ctx.storage.history_distinct_users().await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(kb = %kb_name, error = %e, "history_distinct_users failed");
                    Vec::new()
                }
            }
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let observed: Vec<String> = super::buffered_join(futs, state.fanout_cap)
        .await
        .into_iter()
        .flatten()
        .collect();
    for name in observed {
        if name.is_empty() {
            continue;
        }
        map.entry(name.clone())
            .and_modify(|e| e.observed = true)
            .or_insert_with(|| UserEntry {
                name,
                display: None,
                configured: false,
                observed: true,
            });
    }

    let users: Vec<UserEntry> = map.into_values().collect();
    Json(UsersResponse { users })
}
