//! `kb download [<id-or-path>] [--folder <path> | --all] [--kb] [-o FILE]`
//! — pull an artifact's raw source, or a folder / whole-kb as a `.zip`,
//! over the daemon HTTP API.
//!
//! Pipe-friendly: bytes go to stdout by default (`kb download --folder x
//! | tar`, `… > out.zip`, `… | sha256sum`); `-o FILE` writes a file
//! instead. A binary `.zip` is refused when stdout is a terminal — pass
//! `-o` or pipe it. Single HTML to a terminal is fine.
//!
//! Reuses the server endpoints the SPA download controls hit:
//!   GET /api/kb/{kb}/artifact/{id}        (single, raw source bytes)
//!   GET /api/kb/{kb}/download?folder=<f>  (folder/whole-kb .zip)

use anyhow::{anyhow, Context, Result};
use std::io::{IsTerminal, Write};
use std::path::Path;

const DEFAULT_DAEMON: &str = "http://127.0.0.1:4000";
/// Generous timeout — a whole-kb zip can take a moment to build + stream.
const TIMEOUT_SECS: u64 = 60;

pub async fn run(
    target: Option<&str>,
    folder: Option<&str>,
    all: bool,
    kb: Option<&str>,
    out: Option<&Path>,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    // clap marks target/folder/all mutually exclusive; this guards the
    // "none given" case.
    if target.is_none() && folder.is_none() && !all {
        return Err(anyhow!(
            "nothing to download: pass <id-or-path>, --folder <path>, or --all"
        ));
    }

    let base = daemon.unwrap_or(DEFAULT_DAEMON).trim_end_matches('/');
    let kb = crate::http::resolve_default_kb(kb, daemon, bearer).await?;
    let client = crate::http::client_with_timeout_and_bearer(TIMEOUT_SECS, bearer)?;

    let (bytes, is_zip) = if let Some(t) = target {
        let id = resolve_artifact_id(&kb, t, daemon, bearer).await?;
        let url = format!(
            "{base}/api/kb/{}/artifact/{}",
            crate::http::encode_path_segment(&kb),
            crate::http::encode_path_segment(&id),
        );
        let bytes = fetch_bytes(&client, &url, None).await?;
        (bytes, false)
    } else {
        // Folder or whole-kb zip. `--all` (or `--folder` absent) → no
        // folder query param → the server archives the whole kb.
        let url = format!(
            "{base}/api/kb/{}/download",
            crate::http::encode_path_segment(&kb),
        );
        let query = folder.map(|f| ("folder", f));
        let bytes = fetch_bytes(&client, &url, query).await?;
        (bytes, true)
    };

    write_out(&bytes, out, is_zip)
}

/// Resolve a positional `<id-or-path>` to a 12-hex artifact id. A bare
/// 12-hex id is used as-is (no round-trip); anything else is resolved via
/// `/lookup` — mirrors `comments.rs::resolve_target`.
async fn resolve_artifact_id(
    kb: &str,
    target: &str,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<String> {
    if is_hex12(target) {
        return Ok(target.to_string());
    }
    let body = crate::http::get_lookup(daemon, kb, target, bearer).await?;
    let kind = body
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    match kind {
        "exact" | "unique_suffix" => body
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| anyhow!("lookup response missing `id`")),
        "ambiguous" => {
            let candidates = body
                .get("candidates")
                .and_then(|v| v.as_array())
                .map(|a| a.as_slice())
                .unwrap_or(&[]);
            let mut msg = format!(
                "{target:?} matched {} artifacts in {kb}; pick one:\n",
                candidates.len()
            );
            for c in candidates {
                let id = c.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                let rel = c
                    .get("source_relative")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                msg.push_str(&format!("  {id}  {rel}\n"));
            }
            Err(anyhow!(msg))
        }
        "not_found" => Err(anyhow!("{target:?} matched no artifact in {kb}")),
        other => Err(anyhow!("lookup returned unknown kind {other:?}: {body}")),
    }
}

fn is_hex12(s: &str) -> bool {
    s.len() == 12 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

async fn fetch_bytes(
    client: &reqwest::Client,
    url: &str,
    query: Option<(&str, &str)>,
) -> Result<Vec<u8>> {
    let mut req = client.get(url);
    if let Some((k, v)) = query {
        req = req.query(&[(k, v)]);
    }
    let resp = req.send().await.with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        // Surface the daemon's problem+json `detail` when present.
        let text = resp.text().await.unwrap_or_default();
        let detail = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v.get("detail").and_then(|d| d.as_str()).map(str::to_string))
            .unwrap_or(text);
        return Err(anyhow!("GET {url}: HTTP {status} — {detail}"));
    }
    Ok(resp.bytes().await?.to_vec())
}

/// Write bytes to `out` (file) or stdout. Refuses to dump a binary `.zip`
/// to a terminal — the user almost certainly meant `-o` or a pipe.
fn write_out(bytes: &[u8], out: Option<&Path>, is_zip: bool) -> Result<()> {
    match out {
        Some(path) => {
            std::fs::write(path, bytes).with_context(|| format!("write {}", path.display()))?;
            eprintln!(
                "kb download: wrote {} ({} bytes)",
                path.display(),
                bytes.len()
            );
            Ok(())
        }
        None => {
            let mut stdout = std::io::stdout();
            if is_zip && stdout.is_terminal() {
                return Err(anyhow!(
                    "refusing to write a .zip to a terminal; redirect with `-o FILE` or pipe it"
                ));
            }
            stdout.write_all(bytes).context("write to stdout")?;
            stdout.flush().context("flush stdout")?;
            Ok(())
        }
    }
}
