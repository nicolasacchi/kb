//! `kb prompt <id> [--kb] [--raw|--json]` — read an artifact's stored
//! generation prompt (the `<template id="kb-prompt">` bundle it was
//! authored with, 8 KiB-capped at index time). Talks to the daemon's
//! `GET /api/kb/{kb}/artifacts/{id}/prompt`.
//!
//! **LOCAL-RENDER ONLY**: a loopback daemon (the default) always serves the
//! prompt verbatim. Pointing this at a REMOTE daemon (`--daemon
//! http://...`) whose kb has `[kb.*.outbound] strip_kb_prompt = true`
//! configured gets the exact same honest `stripped: true` (no text) a
//! browser fetch would — that's the scrub gate working as designed
//! (CLAUDE.md invariant #5), not something to route around.

use anyhow::{anyhow, Result};
use serde_json::Value;

use crate::http;

pub async fn run(
    id: &str,
    kb: Option<&str>,
    raw: bool,
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
        "{url}/api/kb/{}/artifacts/{}/prompt",
        http::encode_path_segment(&kb_name),
        http::encode_path_segment(id),
    );
    let resp = client.get(&req_url).send().await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        let detail = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v.get("detail").and_then(|d| d.as_str()).map(str::to_string))
            .unwrap_or(text);
        return Err(anyhow!(
            "artifact {id} not found in kb {kb_name} via daemon {url}: HTTP {status} — {detail}"
        ));
    }
    let body: Value = resp.json().await?;

    if json {
        // Echo the daemon body verbatim + the resolved kb/source, mirroring
        // `kb resurface --json`.
        let mut out = body.clone();
        if let Some(obj) = out.as_object_mut() {
            obj.insert("kb".into(), Value::String(kb_name));
            obj.insert("source".into(), Value::String(url));
        }
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    let stripped = body["stripped"].as_bool().unwrap_or(false);
    let prompt = body["prompt"].as_str();
    let size_bytes = body["size_bytes"].as_u64().unwrap_or(0);

    if raw {
        // Script-friendly: the prompt text and nothing else, or nothing at
        // all when withheld/absent — mirrors `kb cat`'s bytes-only stdout.
        if let Some(p) = prompt {
            print!("{p}");
        }
        return Ok(());
    }

    if stripped {
        println!("prompt withheld on this corpus for non-local readers  [{kb_name}] {id}");
        return Ok(());
    }
    match prompt {
        Some(p) => {
            println!("prompt · {size_bytes} bytes  [{kb_name}] {id}");
            println!("{p}");
        }
        None => println!("no prompt stored for {id} in [{kb_name}]"),
    }
    Ok(())
}
