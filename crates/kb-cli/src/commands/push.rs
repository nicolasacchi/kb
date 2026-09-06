//! `kb push [--filter EVENTKIND] [--daemon URL]` — Claude-Code-friendly
//! SSE consumer. Tails `/api/events`, prints each event as a markdown
//! block to stdout. Reconnects on disconnect with `Last-Event-ID` +
//! exponential backoff (1/2/4/8/30s capped).
//!
//! Pipe to `claude code -- ...` to feed Claude a live stream of kb
//! activity. Filter by event kind to keep the prompt-window focused
//! (e.g. `--filter artifact.indexed --filter comment.anchor_stale`).

use crate::sse::{tail_events, TailOpts};
use anyhow::Result;

pub async fn run(filter: &[String], daemon: Option<&str>, bearer: Option<&str>) -> Result<()> {
    let base = daemon
        .unwrap_or("http://127.0.0.1:4000")
        .trim_end_matches('/');
    // v0.24 T1 (D7): the reconnect/backoff/resume loop lives in
    // `sse::tail_events`, shared with `kb events --follow`. kb push
    // stays a single-daemon, exact-match pipe: per-frame it applies the
    // client-side `--filter` kind list and prints markdown blocks.
    let filter = filter.to_vec();
    tail_events(
        TailOpts {
            base,
            bearer,
            query: "",
            label: "kb push",
        },
        move |frame| {
            let kind = match frame.event.as_deref() {
                Some(k) => k,
                None => return,
            };
            if !filter.is_empty() && !filter.iter().any(|f| f == kind) {
                return;
            }
            // LOW (deep-review): pre-fix used `unwrap_or_default()` which
            // silently mapped malformed JSON to `Value::Null`, and downstream
            // formatters then showed "?" for every field. Now log the parse
            // error to stderr (`kb push` prints stream blocks to stdout so
            // stderr is the right channel for parse warnings) and surface
            // the raw payload so consumers still see *something*.
            let raw = frame.data.as_deref().unwrap_or("{}");
            let envelope: serde_json::Value = match serde_json::from_str(raw) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("kb push: malformed payload for {kind}: {e}");
                    serde_json::json!({ "raw": raw })
                }
            };
            let payload = event_payload(envelope);
            print_block(kind, &payload);
        },
    )
    .await
}

/// SSE frames wrap the event data in `{ payload: {...}, ts, v }`; the
/// summarise/hint formatters read the inner fields flat (kb,
/// artifact_id, …). Unwrap the envelope, falling back to the whole
/// value for a malformed frame or a future flattened emit.
fn event_payload(envelope: serde_json::Value) -> serde_json::Value {
    match envelope.get("payload") {
        Some(inner) => inner.clone(),
        None => envelope,
    }
}

fn print_block(kind: &str, payload: &serde_json::Value) {
    let summary = summarise(kind, payload);
    let hint = hint_for(kind, payload);
    println!("kb event {kind}: {summary}");
    if let Some(h) = hint {
        println!("  {h}");
    }
    println!();
}

/// Render a one-line human summary per event kind. Falls back to the
/// raw payload when the kind is unrecognised so consumers still see
/// something useful for novel events.
fn summarise(kind: &str, payload: &serde_json::Value) -> String {
    match kind {
        "artifact.indexed" => format!(
            "{} (kb={}, hash={})",
            payload["path"].as_str().unwrap_or("?"),
            payload["kb"].as_str().unwrap_or("?"),
            short_hash(payload["hash"].as_str().unwrap_or("")),
        ),
        "artifact.removed" => format!(
            "{} (kb={})",
            payload["path"].as_str().unwrap_or("?"),
            payload["kb"].as_str().unwrap_or("?"),
        ),
        "comment.anchor_stale" | "comment.anchor_resolved" => format!(
            "comment {} on artifact {} (kb={})",
            payload["comment_id"].as_str().unwrap_or("?"),
            short_hash(payload["artifact_id"].as_str().unwrap_or("")),
            payload["kb"].as_str().unwrap_or("?"),
        ),
        "comments.updated" => format!(
            "comments updated on artifact {} (kb={}, open={})",
            short_hash(payload["artifact_id"].as_str().unwrap_or("")),
            payload["kb"].as_str().unwrap_or("?"),
            payload["open_count"].as_u64().unwrap_or(0),
        ),
        "history.recorded" => format!(
            "history {} (kb={}, artifact={})",
            payload["kind"].as_str().unwrap_or("?"),
            payload["kb"].as_str().unwrap_or("?"),
            short_hash(payload["artifact_id"].as_str().unwrap_or("")),
        ),
        "atlas.recompute.complete" => format!(
            "{} points in {} clusters ({} ms)",
            payload["points"].as_u64().unwrap_or(0),
            payload["clusters"].as_u64().unwrap_or(0),
            payload["duration_ms"].as_u64().unwrap_or(0),
        ),
        "index.complete" => format!(
            "run {} · ok={} err={} ({} ms)",
            payload["run"].as_str().unwrap_or("?"),
            payload["ok_count"].as_u64().unwrap_or(0),
            payload["err_count"].as_u64().unwrap_or(0),
            payload["duration_ms"].as_u64().unwrap_or(0),
        ),
        _ => payload.to_string(),
    }
}

/// Suggest a follow-up `kb` command Claude can run next.
fn hint_for(kind: &str, payload: &serde_json::Value) -> Option<String> {
    let kb = payload["kb"].as_str()?;
    let id = payload["artifact_id"].as_str();
    match kind {
        "artifact.indexed" => {
            // The hash is the artifact id; surface a `kb get`.
            payload["hash"].as_str().map(|h| {
                format!("`kb get {h} --kb {kb}` to fetch · `kb related {h} --kb {kb}` for links")
            })
        }
        "comment.anchor_stale" => id.map(|aid| {
            format!(
                "`kb comments list --kb {kb}` · `kb get {aid} --kb {kb} --format html` to inspect"
            )
        }),
        _ => None,
    }
}

fn short_hash(s: &str) -> String {
    // Byte-cap 12 with char-boundary snap — artifact ids are usually hex,
    // but a non-ASCII id must not panic mid-codepoint.
    kb_core::strutil::truncate_bytes_ellipsis(s, 12)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn event_payload_unwraps_envelope() {
        // Real SSE shape: `{ payload: {...}, ts, v }`.
        let envelope = json!({
            "payload": {"kb": "smoke", "artifact_id": "abc123", "open_count": 2},
            "ts": "2026-05-20T10:00:00Z",
            "v": 1
        });
        let p = event_payload(envelope);
        assert_eq!(p["kb"], "smoke");
        assert_eq!(p["artifact_id"], "abc123");
        // And the formatter now reads the real kb instead of "?".
        assert!(summarise("comments.updated", &p).contains("kb=smoke"));
    }

    #[test]
    fn event_payload_passes_through_flat_value() {
        // Malformed-frame fallback (`{raw:...}`) and any flattened emit
        // are returned unchanged.
        let flat = json!({"raw": "not json"});
        assert_eq!(event_payload(flat.clone()), flat);
    }

    #[test]
    fn short_hash_under_budget_unchanged() {
        assert_eq!(short_hash("abc"), "abc");
        assert_eq!(short_hash("abcdefghijkl"), "abcdefghijkl");
    }

    #[test]
    fn short_hash_multibyte_does_not_panic() {
        // Each CJK char is 3 bytes; max 12 bytes → 4 chars, no mid-char slice.
        let s = "日本語クエリ拡張";
        let out = short_hash(s);
        assert!(out.ends_with('…'));
        assert!(out.is_char_boundary(out.len()));
        // Under 12 bytes: keep whole string.
        assert_eq!(short_hash("日本語"), "日本語");
    }
}
