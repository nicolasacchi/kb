//! `kb resurface` — the deterministic, pull-only resurfacing queue: artifacts
//! with open comments and unfinished reads, scored and explained. Talks to
//! the daemon's `GET /api/kb/{kb}/resurface`. Reasons always print (the
//! recollect house style); `--explain` adds the per-item arithmetic rendered
//! from the weighted terms the server returns (`comment_term + read_term ==
//! score`, so no re-derivation happens client-side). Design:
//! `docs/research/kb-resurface-queue-2026-07.html`.

use anyhow::{anyhow, Result};
use kb_core::resurface::ResurfaceWeights;
use serde_json::Value;

use crate::http;

pub async fn run(
    kb: Option<&str>,
    limit: u32,
    explain: bool,
    json: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let url = http::detect_daemon(daemon, bearer).await.ok_or_else(|| {
        anyhow!(
            "daemon not reachable{} — start it with `kb daemon`",
            daemon.map(|d| format!(" at {d}")).unwrap_or_default()
        )
    })?;
    let kb_name = http::resolve_default_kb(kb, daemon, bearer).await?;
    let client = http::client_with_timeout_and_bearer(10, bearer)?;
    let req_url = format!(
        "{url}/api/kb/{}/resurface?limit={limit}",
        http::encode_path_segment(&kb_name),
    );
    let body: Value = client
        .get(&req_url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    if json {
        // Echo the daemon body verbatim + the resolved kb/source, mirroring
        // `kb reading --json` so an agent gets a self-contained object.
        let mut out = body.clone();
        if let Some(obj) = out.as_object_mut() {
            obj.insert("kb".into(), Value::String(kb_name));
            obj.insert("source".into(), Value::String(url));
        }
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    print_human(&body, &kb_name, explain);
    Ok(())
}

fn print_human(body: &Value, kb: &str, explain: bool) {
    let items = body["items"].as_array().cloned().unwrap_or_default();
    if items.is_empty() {
        // Deliberately quiet — no celebration, no streaks (the design's
        // anti-feature list). An empty queue is just an empty queue.
        println!("resurface  [{kb}]  nothing to pick up");
        return;
    }
    println!(
        "resurface  [{kb}]  {} item{}  (pull-only; acting on an item clears it)",
        items.len(),
        if items.len() == 1 { "" } else { "s" }
    );
    let weights = resolve_weights(body);
    for (i, it) in items.iter().enumerate() {
        let title = it["title"].as_str().unwrap_or("?");
        let id = it["id"].as_str().unwrap_or("?");
        let score = it["score"].as_f64().unwrap_or(0.0);
        println!("{:>2}. {score:.3}  {title}  {id}", i + 1);
        if let Some(reasons) = it["reasons"].as_array() {
            for r in reasons {
                if let Some(s) = r.as_str() {
                    println!("           ▸ {s}");
                }
            }
        }
        if explain {
            println!("           = {}", explain_line(it, &weights));
        }
    }
}

/// Read the daemon's `weights` object off the response body, falling back
/// to [`ResurfaceWeights::default`] field-by-field — an older daemon that
/// predates W2.9 sends no `weights` key at all, and `--explain` should
/// still render (with the shipped defaults, which is exactly what that
/// daemon is actually scoring with).
fn resolve_weights(body: &Value) -> ResurfaceWeights {
    let d = ResurfaceWeights::default();
    let w = &body["weights"];
    ResurfaceWeights {
        comment_weight: w["comment_weight"]
            .as_f64()
            .map(|v| v as f32)
            .unwrap_or(d.comment_weight),
        read_weight: w["read_weight"]
            .as_f64()
            .map(|v| v as f32)
            .unwrap_or(d.read_weight),
        comment_saturation: w["comment_saturation"]
            .as_u64()
            .map(|v| v as u32)
            .unwrap_or(d.comment_saturation),
        read_halflife_days: w["read_halflife_days"]
            .as_f64()
            .map(|v| v as f32)
            .unwrap_or(d.read_halflife_days),
        score_floor: d.score_floor,
    }
}

/// Render the scoring arithmetic from the server's weighted terms — e.g.
/// `0.6·min(2,4)/4 = 0.300  +  0.4·62%·decay = 0.229  →  0.529`. The
/// multipliers/saturation/half-life come from `weights` (the response's
/// REAL per-kb values), never a hardcoded literal, so a tuned kb's
/// `--explain` output can't drift from what the server actually computed.
fn explain_line(it: &Value, weights: &ResurfaceWeights) -> String {
    let ct = it["comment_term"].as_f64().unwrap_or(0.0);
    let rt = it["read_term"].as_f64().unwrap_or(0.0);
    let score = it["score"].as_f64().unwrap_or(0.0);
    let open = it["open_comments"].as_u64().unwrap_or(0);
    let mut parts = Vec::new();
    if open > 0 {
        let sat = weights.comment_saturation;
        parts.push(format!(
            "{}·min({open},{sat})/{sat} = {ct:.3}",
            weights.comment_weight
        ));
    }
    if let (Some(pct), Some(_)) = (
        it["completion_pct"].as_u64(),
        it["last_opened_unix"].as_i64(),
    ) {
        parts.push(format!(
            "{}·{pct}%·0.5^(idle/{}d) = {rt:.3}",
            weights.read_weight, weights.read_halflife_days
        ));
    }
    format!("{}  →  {score:.3}", parts.join("  +  "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explain_line_renders_both_terms() {
        let it: Value = serde_json::json!({
            "comment_term": 0.3, "read_term": 0.229, "score": 0.529,
            "open_comments": 2, "completion_pct": 62,
            "last_opened_unix": 1_780_000_000_i64,
        });
        assert_eq!(
            explain_line(&it, &ResurfaceWeights::default()),
            "0.6·min(2,4)/4 = 0.300  +  0.4·62%·0.5^(idle/45d) = 0.229  →  0.529"
        );
    }

    #[test]
    fn explain_line_comment_only() {
        let it: Value = serde_json::json!({
            "comment_term": 0.15, "read_term": 0.0, "score": 0.15,
            "open_comments": 1,
        });
        assert_eq!(
            explain_line(&it, &ResurfaceWeights::default()),
            "0.6·min(1,4)/4 = 0.150  →  0.150"
        );
    }

    // W2.9 — a tuned kb's `--explain` output must show ITS weights, not
    // the shipped defaults, proving the renderer genuinely reads `weights`
    // rather than a hardcoded literal.
    #[test]
    fn explain_line_renders_custom_weights() {
        let it: Value = serde_json::json!({
            "comment_term": 0.4, "read_term": 0.1, "score": 0.5,
            "open_comments": 3, "completion_pct": 50,
            "last_opened_unix": 1_780_000_000_i64,
        });
        let w = ResurfaceWeights {
            comment_weight: 0.8,
            read_weight: 0.2,
            comment_saturation: 6,
            read_halflife_days: 20.0,
            score_floor: 0.05,
        };
        assert_eq!(
            explain_line(&it, &w),
            "0.8·min(3,6)/6 = 0.400  +  0.2·50%·0.5^(idle/20d) = 0.100  →  0.500"
        );
    }

    #[test]
    fn resolve_weights_falls_back_to_defaults_when_absent() {
        let body = serde_json::json!({ "items": [], "now_unix": 0 });
        assert_eq!(resolve_weights(&body), ResurfaceWeights::default());
    }

    #[test]
    fn resolve_weights_reads_the_wire_object() {
        let body = serde_json::json!({
            "items": [], "now_unix": 0,
            "weights": {
                "comment_weight": 0.8, "read_weight": 0.2,
                "comment_saturation": 6, "read_halflife_days": 20.0,
            },
        });
        let w = resolve_weights(&body);
        assert_eq!(w.comment_weight, 0.8);
        assert_eq!(w.read_weight, 0.2);
        assert_eq!(w.comment_saturation, 6);
        assert_eq!(w.read_halflife_days, 20.0);
    }
}
