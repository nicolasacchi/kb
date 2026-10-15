//! `GET /api/settings` and `PATCH /api/settings` — daemon-level UI prefs
//! per topic 11 §B.1 (cluster-4 hybrid-persistence). PATCH updates the
//! in-memory UiSection only in v0.1; durable writes to kb.toml defer to
//! v0.2 (the SPA's localStorage is the durable cache in the meantime).
//!
//! OL S1 adds `theme_family` (the kbc-theme/1 palette family): the SPA's
//! picker PATCHes it, and the sibling kb-code daemon reads it back through
//! its `GET /api/kb/ui` relay so its SPA can follow kb's choice.

use crate::state::KbHandles;
use axum::{body::Body, extract::State, http::Response, response::IntoResponse, Json};
use kb_core::config::UiSection;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Serialize, Default)]
pub struct SettingsResponse {
    pub theme: Option<String>,
    pub accent: Option<String>,
    pub density: Option<String>,
    pub theme_family: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct SettingsPatch {
    pub theme: Option<String>,
    pub accent: Option<String>,
    pub density: Option<String>,
    /// Also accepted as `themeFamily` (the SPA's prefs key).
    #[serde(default, alias = "themeFamily")]
    pub theme_family: Option<String>,
}

/// A palette family id: `[a-z0-9-]{1,40}`. The daemon does not own the
/// registry (it is a generated TS table in `ui/`), so it validates the
/// SHAPE only; a well-formed id no client knows is simply ignored by them.
pub fn valid_theme_family(s: &str) -> bool {
    (1..=40).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn snapshot(ui: &UiSection) -> SettingsResponse {
    SettingsResponse {
        theme: ui.theme.clone(),
        accent: ui.accent.clone(),
        density: ui.density.clone(),
        theme_family: ui.theme_family.clone(),
    }
}

/// Apply a patch to the in-memory prefs. Absent fields leave the stored
/// value alone; a malformed `theme_family` is ignored (stored value kept).
pub fn apply_patch(ui: &mut UiSection, body: SettingsPatch) {
    if body.theme.is_some() {
        ui.theme = body.theme;
    }
    if body.accent.is_some() {
        ui.accent = body.accent;
    }
    if body.density.is_some() {
        ui.density = body.density;
    }
    if let Some(family) = body.theme_family {
        if valid_theme_family(&family) {
            ui.theme_family = Some(family);
        }
    }
}

pub async fn get(State(state): State<Arc<KbHandles>>) -> Json<SettingsResponse> {
    let ui = state.ui.lock().unwrap_or_else(|e| e.into_inner());
    Json(snapshot(&ui))
}

pub async fn patch(
    State(state): State<Arc<KbHandles>>,
    Json(body): Json<SettingsPatch>,
) -> Response<Body> {
    let mut ui = state.ui.lock().unwrap_or_else(|e| e.into_inner());
    apply_patch(&mut ui, body);
    let snapshot = snapshot(&ui);
    drop(ui);
    Json(snapshot).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_shape_is_the_closed_charset() {
        for ok in ["kb-classic", "catppuccin", "a", "x1-2"] {
            assert!(valid_theme_family(ok), "{ok}");
        }
        let too_long = "a".repeat(41);
        for bad in ["", "Kb", "a b", "a_b", "../x", "<b>", too_long.as_str()] {
            assert!(!valid_theme_family(bad), "{bad}");
        }
    }

    #[test]
    fn patch_sets_a_valid_family_and_ignores_a_malformed_one() {
        let mut ui = UiSection::default();
        apply_patch(
            &mut ui,
            SettingsPatch {
                theme_family: Some("catppuccin".into()),
                ..Default::default()
            },
        );
        assert_eq!(ui.theme_family.as_deref(), Some("catppuccin"));

        // Malformed: ignored, the previous value stays.
        apply_patch(
            &mut ui,
            SettingsPatch {
                theme_family: Some("<script>".into()),
                ..Default::default()
            },
        );
        assert_eq!(ui.theme_family.as_deref(), Some("catppuccin"));

        // Absent: untouched, and the other fields still apply.
        apply_patch(
            &mut ui,
            SettingsPatch {
                theme: Some("dark".into()),
                ..Default::default()
            },
        );
        assert_eq!(ui.theme_family.as_deref(), Some("catppuccin"));
        assert_eq!(ui.theme.as_deref(), Some("dark"));
    }

    #[test]
    fn the_spa_prefs_key_is_accepted_as_an_alias() {
        let p: SettingsPatch =
            serde_json::from_str(r#"{"theme":"dark","themeFamily":"kb-classic"}"#).unwrap();
        assert_eq!(p.theme_family.as_deref(), Some("kb-classic"));
        let p: SettingsPatch = serde_json::from_str(r#"{"theme_family":"kb-classic"}"#).unwrap();
        assert_eq!(p.theme_family.as_deref(), Some("kb-classic"));
    }
}
