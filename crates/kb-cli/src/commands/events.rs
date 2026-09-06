//! `kb events --follow [--types G]… [--kb NAME] [--artifact ID]` —
//! the fleet-monitor event tail (v0.24 T1, TUI-retirement gap closer).
//!
//! Operator ruling D7: a SEPARATE verb that wraps the same SSE tail
//! loop `kb push` uses (`sse::tail_events` — `Last-Event-ID` reconnect
//! with exponential backoff), NOT an extension of kb push's identity.
//! Where kb push is a Claude-prompt formatter with client-side
//! exact-kind filtering, `kb events` is an operator tail: filters are
//! applied SERVER-SIDE (`?types=` globs + the `?filter=` payload
//! tokens `kb:`/`artifact:` this milestone implements), and the output
//! is one line per event — `id  type  payload` by default, NDJSON
//! envelopes with `--json`.

use crate::http::encode_path_segment;
use crate::sse::{tail_events, Frame, TailOpts};
use anyhow::Result;

pub struct EventsArgs<'a> {
    /// Event-type globs, joined into the server's `?types=` list
    /// (e.g. `index.*`, `error`). Empty = every type.
    pub types: &'a [String],
    /// Server-side `kb:<name>` payload filter.
    pub kb: Option<&'a str>,
    /// Server-side `artifact:<id>` payload filter.
    pub artifact: Option<&'a str>,
    pub daemon: Option<&'a str>,
    pub bearer: Option<&'a str>,
    /// NDJSON envelopes (`{id, type, ts, v, payload}`) instead of the
    /// human `id  type  payload` lines.
    pub json: bool,
}

pub async fn follow(args: EventsArgs<'_>) -> Result<()> {
    let base = args
        .daemon
        .unwrap_or("http://127.0.0.1:4000")
        .trim_end_matches('/');
    let query = build_query(args.types, args.kb, args.artifact);
    let json = args.json;
    tail_events(
        TailOpts {
            base,
            bearer: args.bearer,
            query: &query,
            label: "kb events",
        },
        move |frame| print_frame(frame, json),
    )
    .await
}

/// Build the pre-encoded `/api/events` query string. Values ride
/// through `encode_path_segment` — a superset of query-value escaping
/// (axum percent-decodes either way), so a hostile shell arg can't
/// smuggle an `&` into a second parameter. Commas separating tokens
/// stay literal (the server splits the DECODED value on `,`).
fn build_query(types: &[String], kb: Option<&str>, artifact: Option<&str>) -> String {
    let mut parts: Vec<String> = Vec::new();
    let type_list: Vec<String> = types
        .iter()
        .flat_map(|t| t.split(','))
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(encode_path_segment)
        .collect();
    if !type_list.is_empty() {
        parts.push(format!("types={}", type_list.join(",")));
    }
    let mut filter_tokens: Vec<String> = Vec::new();
    if let Some(name) = kb {
        filter_tokens.push(format!("kb:{}", encode_path_segment(name)));
    }
    if let Some(id) = artifact {
        filter_tokens.push(format!("artifact:{}", encode_path_segment(id)));
    }
    if !filter_tokens.is_empty() {
        parts.push(format!("filter={}", filter_tokens.join(",")));
    }
    parts.join("&")
}

/// One line per frame. Keep-alive comment frames arrive with no event
/// AND no data — skip those; synthetic `lag`/`gap` frames (no id) are
/// printed like any event so a consumer sees the discontinuity.
fn print_frame(frame: &Frame, json: bool) {
    let kind = match frame.event.as_deref() {
        Some(k) => k,
        None => return,
    };
    let raw = frame.data.as_deref().unwrap_or("{}");
    if let Some(line) = format_line(frame.id.as_deref(), kind, raw, json) {
        println!("{line}");
    }
}

/// Pure formatter, split out of `print_frame` so tests don't need a
/// live stream. Always returns `Some` today; the `Option` keeps the
/// door open for suppressing frame kinds without touching the caller.
fn format_line(id: Option<&str>, kind: &str, raw_data: &str, json: bool) -> Option<String> {
    let envelope: serde_json::Value = match serde_json::from_str(raw_data) {
        Ok(v) => v,
        Err(_) => serde_json::json!({ "raw": raw_data }),
    };
    if json {
        // NDJSON: flatten the SSE frame + wire envelope into one object.
        let mut out = serde_json::Map::new();
        if let Some(id) = id {
            out.insert("id".into(), serde_json::json!(id));
        }
        out.insert("type".into(), serde_json::json!(kind));
        for key in ["ts", "v", "payload", "raw"] {
            if let Some(v) = envelope.get(key) {
                out.insert(key.to_string(), v.clone());
            }
        }
        // lag/gap frames carry flat data ({skipped} / {requested_id, …})
        // rather than the {payload, ts, v} envelope — pass it through.
        if !out.contains_key("payload") && !out.contains_key("raw") {
            out.insert("payload".into(), envelope);
        }
        return Some(serde_json::Value::Object(out).to_string());
    }
    // Human: `id  type  compact-payload` (lag/gap have no id → `-`).
    let payload = envelope.get("payload").unwrap_or(&envelope);
    Some(format!(
        "{:>8}  {:<28} {}",
        id.unwrap_or("-"),
        kind,
        payload
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_query_joins_types_and_filter_tokens() {
        let types = vec!["index.*".to_string(), "error,query".to_string()];
        let q = build_query(&types, Some("docs"), Some("abc123def456"));
        assert_eq!(
            q,
            "types=index.%2A,error,query&filter=kb:docs,artifact:abc123def456"
        );
    }

    #[test]
    fn build_query_empty_when_unfiltered() {
        assert_eq!(build_query(&[], None, None), "");
    }

    #[test]
    fn build_query_escapes_ampersand_in_values() {
        // A hostile value can't smuggle a second query parameter.
        let q = build_query(&[], Some("a&filter=artifact:x"), None);
        assert!(!q.contains('&'), "raw & must be escaped: {q}");
    }

    #[test]
    fn format_line_human_unwraps_envelope() {
        let line = format_line(
            Some("42"),
            "artifact.indexed",
            r#"{"payload":{"kb":"smoke","path":"a.html"},"ts":"2026-07-12T10:00:00Z","v":1}"#,
            false,
        )
        .unwrap();
        assert!(line.contains("artifact.indexed"), "{line}");
        assert!(line.contains(r#""kb":"smoke""#), "{line}");
        assert!(line.trim_start().starts_with("42"), "{line}");
    }

    #[test]
    fn format_line_json_is_parseable_ndjson_with_type_and_id() {
        let line = format_line(
            Some("7"),
            "comments.updated",
            r#"{"payload":{"kb":"smoke","open_count":2},"ts":"2026-07-12T10:00:00Z","v":1}"#,
            true,
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["id"], "7");
        assert_eq!(v["type"], "comments.updated");
        assert_eq!(v["payload"]["kb"], "smoke");
        assert_eq!(v["v"], 1);
    }

    #[test]
    fn format_line_json_passes_flat_lag_frame_through() {
        // Synthetic lag/gap frames have flat data + no id.
        let line = format_line(None, "lag", r#"{"skipped":12}"#, true).unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["type"], "lag");
        assert_eq!(v["payload"]["skipped"], 12);
        assert!(v.get("id").is_none());
    }
}
