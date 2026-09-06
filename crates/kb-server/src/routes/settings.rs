//! `GET /api/settings` and `PATCH /api/settings` — daemon-level UI prefs
//! per topic 11 §B.1 (cluster-4 hybrid-persistence). PATCH updates the
//! in-memory UiSection only in v0.1; durable writes to kb.toml defer to
//! v0.2 (the SPA's localStorage is the durable cache in the meantime).

use crate::state::KbHandles;
use axum::{body::Body, extract::State, http::Response, response::IntoResponse, Json};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Serialize, Default)]
pub struct SettingsResponse {
    pub theme: Option<String>,
    pub accent: Option<String>,
    pub density: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct SettingsPatch {
    pub theme: Option<String>,
    pub accent: Option<String>,
    pub density: Option<String>,
}

pub async fn get(State(state): State<Arc<KbHandles>>) -> Json<SettingsResponse> {
    let ui = state.ui.lock().unwrap_or_else(|e| e.into_inner());
    Json(SettingsResponse {
        theme: ui.theme.clone(),
        accent: ui.accent.clone(),
        density: ui.density.clone(),
    })
}

pub async fn patch(
    State(state): State<Arc<KbHandles>>,
    Json(body): Json<SettingsPatch>,
) -> Response<Body> {
    let mut ui = state.ui.lock().unwrap_or_else(|e| e.into_inner());
    if body.theme.is_some() {
        ui.theme = body.theme;
    }
    if body.accent.is_some() {
        ui.accent = body.accent;
    }
    if body.density.is_some() {
        ui.density = body.density;
    }
    let snapshot = SettingsResponse {
        theme: ui.theme.clone(),
        accent: ui.accent.clone(),
        density: ui.density.clone(),
    };
    drop(ui);
    Json(snapshot).into_response()
}
